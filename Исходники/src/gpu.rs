//! D3D11 device creation and GPU colour conversion / scaling (BGRA → NV12, BT.709 limited).

use std::collections::HashMap;
use std::mem::ManuallyDrop;

use windows::core::{Interface, Result};
use windows::Win32::Foundation::{BOOL, RECT};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::System::Threading::GetCurrentProcess;

pub struct Gpu {
    pub device: ID3D11Device,
    pub context: ID3D11DeviceContext,
    pub adapter_name: String,
    pub luid: (u32, i32),
}

impl Gpu {
    /// Creates a device on the high-performance adapter (discrete GPU on hybrid systems).
    pub fn new() -> Result<Gpu> {
        unsafe {
            let factory: IDXGIFactory6 = CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0))?;
            let adapter: IDXGIAdapter1 = factory.EnumAdapterByGpuPreference(0, DXGI_GPU_PREFERENCE_HIGH_PERFORMANCE)?;
            let desc = adapter.GetDesc1()?;
            let name = crate::util::from_wide(&desc.Description);
            let levels = [D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0];
            let mut device = None;
            let mut context = None;
            D3D11CreateDevice(
                &adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                None,
                D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
                Some(&levels),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )?;
            let device: ID3D11Device = device.unwrap();
            let context: ID3D11DeviceContext = context.unwrap();
            // Media Foundation uses the device from its own worker threads.
            if let Ok(mt) = device.cast::<ID3D11Multithread>() {
                let _ = mt.SetMultithreadProtected(true);
            }
            // Keep capture/encode responsive while a game saturates the GPU.
            if let Ok(dxgi) = device.cast::<IDXGIDevice>() {
                let _ = dxgi.SetGPUThreadPriority(7);
            }
            let _ = GetCurrentProcess;
            Ok(Gpu {
                device,
                context,
                adapter_name: name,
                luid: (desc.AdapterLuid.LowPart, desc.AdapterLuid.HighPart),
            })
        }
    }
}

/// Converts BGRA textures of arbitrary size into NV12 at a fixed output size, preserving the
/// aspect ratio (letterboxing with black).
pub struct Converter {
    vdev: ID3D11VideoDevice,
    vctx: ID3D11VideoContext,
    out_w: u32,
    out_h: u32,
    in_w: u32,
    in_h: u32,
    enumerator: Option<ID3D11VideoProcessorEnumerator>,
    processor: Option<ID3D11VideoProcessor>,
    input_view: Option<(usize, ID3D11VideoProcessorInputView)>,
    output_views: HashMap<usize, ID3D11VideoProcessorOutputView>,
}

impl Converter {
    pub fn new(gpu: &Gpu, out_w: u32, out_h: u32) -> Result<Self> {
        Ok(Converter {
            vdev: gpu.device.cast()?,
            vctx: gpu.context.cast()?,
            out_w,
            out_h,
            in_w: 0,
            in_h: 0,
            enumerator: None,
            processor: None,
            input_view: None,
            output_views: HashMap::new(),
        })
    }

    fn ensure(&mut self, in_w: u32, in_h: u32) -> Result<()> {
        if self.processor.is_some() && in_w == self.in_w && in_h == self.in_h {
            return Ok(());
        }
        self.processor = None;
        self.enumerator = None;
        self.input_view = None;
        self.output_views.clear();
        let desc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputFrameRate: DXGI_RATIONAL { Numerator: 60, Denominator: 1 },
            InputWidth: in_w,
            InputHeight: in_h,
            OutputFrameRate: DXGI_RATIONAL { Numerator: 60, Denominator: 1 },
            OutputWidth: self.out_w,
            OutputHeight: self.out_h,
            Usage: D3D11_VIDEO_USAGE_OPTIMAL_QUALITY,
        };
        unsafe {
            let e = self.vdev.CreateVideoProcessorEnumerator(&desc)?;
            let p = self.vdev.CreateVideoProcessor(&e, 0)?;
            // Input: full-range RGB. Output: BT.709 limited range (studio swing).
            let in_cs = D3D11_VIDEO_PROCESSOR_COLOR_SPACE { _bitfield: 0 };
            let out_cs = D3D11_VIDEO_PROCESSOR_COLOR_SPACE { _bitfield: (1 << 2) | (1 << 4) };
            if let Ok(c1) = self.vctx.cast::<ID3D11VideoContext1>() {
                c1.VideoProcessorSetStreamColorSpace1(&p, 0, DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709);
                c1.VideoProcessorSetOutputColorSpace1(&p, DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709);
            } else {
                self.vctx.VideoProcessorSetStreamColorSpace(&p, 0, &in_cs);
                self.vctx.VideoProcessorSetOutputColorSpace(&p, &out_cs);
            }
            self.vctx.VideoProcessorSetStreamFrameFormat(&p, 0, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE);
            self.vctx.VideoProcessorSetStreamAutoProcessingMode(&p, 0, BOOL(0));
            let black = D3D11_VIDEO_COLOR { Anonymous: D3D11_VIDEO_COLOR_0 { RGBA: D3D11_VIDEO_COLOR_RGBA { R: 0.0, G: 0.0, B: 0.0, A: 1.0 } } };
            self.vctx.VideoProcessorSetOutputBackgroundColor(&p, BOOL(0), &black);

            // Letterbox: fit the source into the output keeping aspect ratio, even-aligned.
            let src = RECT { left: 0, top: 0, right: in_w as i32, bottom: in_h as i32 };
            let scale = (self.out_w as f64 / in_w as f64).min(self.out_h as f64 / in_h as f64);
            let dw = ((in_w as f64 * scale).round() as i32).min(self.out_w as i32) & !1;
            let dh = ((in_h as f64 * scale).round() as i32).min(self.out_h as i32) & !1;
            let dx = ((self.out_w as i32 - dw) / 2) & !1;
            let dy = ((self.out_h as i32 - dh) / 2) & !1;
            let dst = RECT { left: dx, top: dy, right: dx + dw, bottom: dy + dh };
            let full = RECT { left: 0, top: 0, right: self.out_w as i32, bottom: self.out_h as i32 };
            self.vctx.VideoProcessorSetStreamSourceRect(&p, 0, BOOL(1), Some(&src));
            self.vctx.VideoProcessorSetStreamDestRect(&p, 0, BOOL(1), Some(&dst));
            self.vctx.VideoProcessorSetOutputTargetRect(&p, BOOL(1), Some(&full));
            self.enumerator = Some(e);
            self.processor = Some(p);
        }
        self.in_w = in_w;
        self.in_h = in_h;
        Ok(())
    }

    /// Converts `src` (BGRA, `in_w`×`in_h`) into `dst` (NV12, output size).
    pub fn convert(&mut self, src: &ID3D11Texture2D, dst: &ID3D11Texture2D, dst_subresource: u32) -> Result<()> {
        let mut d = D3D11_TEXTURE2D_DESC::default();
        unsafe { src.GetDesc(&mut d) };
        self.ensure(d.Width, d.Height)?;
        let e = self.enumerator.clone().unwrap();
        let p = self.processor.clone().unwrap();
        unsafe {
            let src_key = src.as_raw() as usize;
            if self.input_view.as_ref().map(|(k, _)| *k) != Some(src_key) {
                let desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
                    FourCC: 0,
                    ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
                    Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPIV { MipSlice: 0, ArraySlice: 0 } },
                };
                let mut v = None;
                self.vdev.CreateVideoProcessorInputView(src, &e, &desc, Some(&mut v))?;
                self.input_view = Some((src_key, v.unwrap()));
            }
            let dst_key = dst.as_raw() as usize ^ ((dst_subresource as usize) << 48);
            if !self.output_views.contains_key(&dst_key) {
                if self.output_views.len() > 64 {
                    self.output_views.clear();
                }
                let desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                    ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                    Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 } },
                };
                let mut v = None;
                self.vdev.CreateVideoProcessorOutputView(dst, &e, &desc, Some(&mut v))?;
                self.output_views.insert(dst_key, v.unwrap());
            }
            let out_view = self.output_views.get(&dst_key).unwrap();
            let in_view = self.input_view.as_ref().unwrap().1.clone();
            let mut stream = D3D11_VIDEO_PROCESSOR_STREAM {
                Enable: BOOL(1),
                OutputIndex: 0,
                InputFrameOrField: 0,
                PastFrames: 0,
                FutureFrames: 0,
                ppPastSurfaces: std::ptr::null_mut(),
                pInputSurface: ManuallyDrop::new(Some(in_view)),
                ppFutureSurfaces: std::ptr::null_mut(),
                ppPastSurfacesRight: std::ptr::null_mut(),
                pInputSurfaceRight: ManuallyDrop::new(None),
                ppFutureSurfacesRight: std::ptr::null_mut(),
            };
            let r = self.vctx.VideoProcessorBlt(&p, out_view, 0, std::slice::from_ref(&stream));
            ManuallyDrop::drop(&mut stream.pInputSurface);
            r
        }
    }

    /// Fills `dst` with black (used before the first captured frame arrives).
    pub fn forget_input(&mut self) {
        self.input_view = None;
    }
}

/// Picks the encode size for a given source size and resolution setting (even dimensions).
pub fn output_size(src_w: u32, src_h: u32, target_h: Option<u32>) -> (u32, u32) {
    let (w, h) = match target_h {
        Some(h) if h < src_h || src_h == 0 => {
            let w = (src_w as f64 * h as f64 / src_h.max(1) as f64).round() as u32;
            (w, h)
        }
        Some(h) => {
            // Upscaling to a fixed preset is allowed (e.g. 1080p source → 1440p file).
            let w = (src_w as f64 * h as f64 / src_h.max(1) as f64).round() as u32;
            (w, h)
        }
        None => (src_w, src_h),
    };
    ((w.max(64) + 1) & !1, (h.max(64) + 1) & !1)
}

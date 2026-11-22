//! Screen / window capture via Windows.Graphics.Capture (zero-copy D3D11 textures).

pub mod sources;

use windows::core::{factory, Interface, Result};
use windows::Foundation::TypedEventHandler;
use windows::Graphics::Capture::{Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::SizeInt32;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::Graphics::Gdi::HMONITOR;
use windows::Win32::System::WinRT::Direct3D11::{CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub enum Target {
    Monitor(HMONITOR),
    Window(HWND),
}

pub struct Capture {
    _item: GraphicsCaptureItem,
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    winrt_device: IDirect3DDevice,
    pool_size: SizeInt32,
    pub closed: Arc<AtomicBool>,
}

/// Size of the latest captured frame, copied into a texture we own.
pub struct FrameCopy {
    #[allow(dead_code)]
    pub width: u32,
    #[allow(dead_code)]
    pub height: u32,
}

impl Capture {
    pub fn new(device: &ID3D11Device, target: &Target, cursor: bool) -> Result<Self> {
        let interop = factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
        let item: GraphicsCaptureItem = unsafe {
            match target {
                Target::Monitor(m) => interop.CreateForMonitor(*m)?,
                Target::Window(h) => interop.CreateForWindow(*h)?,
            }
        };
        let dxgi: IDXGIDevice = device.cast()?;
        let inspectable = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi)? };
        let winrt_device: IDirect3DDevice = inspectable.cast()?;
        let size = item.Size()?;
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &winrt_device,
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            2,
            size,
        )?;
        let session = pool.CreateCaptureSession(&item)?;
        let _ = session.SetIsCursorCaptureEnabled(cursor);
        // Windows 11: no yellow border around the captured area.
        let _ = session.SetIsBorderRequired(false);
        let closed = Arc::new(AtomicBool::new(false));
        {
            let closed = closed.clone();
            let _ = item.Closed(&TypedEventHandler::new(move |_, _| {
                closed.store(true, Ordering::SeqCst);
                Ok(())
            }));
        }
        session.StartCapture()?;
        Ok(Capture { _item: item, pool, session, winrt_device, pool_size: size, closed })
    }

    /// Drains the frame pool and copies the newest frame into `dst` (recreated when the
    /// content size changes). Returns the content size when a new frame was copied.
    pub fn poll(
        &mut self,
        ctx: &ID3D11DeviceContext,
        device: &ID3D11Device,
        dst: &mut Option<ID3D11Texture2D>,
    ) -> Result<Option<FrameCopy>> {
        let mut latest = None;
        while let Ok(f) = self.pool.TryGetNextFrame() {
            latest = Some(f);
        }
        let Some(frame) = latest else { return Ok(None) };
        let content = frame.ContentSize()?;
        let w = content.Width.max(1) as u32;
        let h = content.Height.max(1) as u32;

        if content.Width != self.pool_size.Width || content.Height != self.pool_size.Height {
            // Window resized / resolution changed: recreate the pool at the new size.
            self.pool_size = content;
            self.pool.Recreate(&self.winrt_device, DirectXPixelFormat::B8G8R8A8UIntNormalized, 2, content)?;
        }

        let surface = frame.Surface()?;
        let access: IDirect3DDxgiInterfaceAccess = surface.cast()?;
        let tex: ID3D11Texture2D = unsafe { access.GetInterface()? };
        let mut sdesc = D3D11_TEXTURE2D_DESC::default();
        unsafe { tex.GetDesc(&mut sdesc) };
        let cw = w.min(sdesc.Width);
        let ch = h.min(sdesc.Height);

        let need_new = match dst {
            Some(t) => {
                let mut d = D3D11_TEXTURE2D_DESC::default();
                unsafe { t.GetDesc(&mut d) };
                d.Width != cw || d.Height != ch
            }
            None => true,
        };
        if need_new {
            let desc = D3D11_TEXTURE2D_DESC {
                Width: cw,
                Height: ch,
                MipLevels: 1,
                ArraySize: 1,
                Format: sdesc.Format,
                SampleDesc: windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
                CPUAccessFlags: 0,
                MiscFlags: 0,
            };
            let mut t = None;
            unsafe { device.CreateTexture2D(&desc, None, Some(&mut t))? };
            *dst = t;
        }
        let b = D3D11_BOX { left: 0, top: 0, front: 0, right: cw, bottom: ch, back: 1 };
        unsafe { ctx.CopySubresourceRegion(dst.as_ref().unwrap(), 0, 0, 0, 0, &tex, 0, Some(&b)) };
        drop(frame);
        Ok(Some(FrameCopy { width: cw, height: ch }))
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        let _ = self.session.Close();
        let _ = self.pool.Close();
    }
}

pub fn is_capture_supported() -> bool {
    GraphicsCaptureSession::IsSupported().unwrap_or(false)
}

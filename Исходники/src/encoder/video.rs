//! Hardware video encoding through Media Foundation transforms.
//!
//! NVIDIA (NVENC), AMD (AMF/VCN) and Intel (Quick Sync) all expose their encoders as hardware
//! MFTs, so one code path covers every vendor. Input is NV12 D3D11 surfaces (zero-copy).

use std::ffi::c_void;

use windows::core::{Interface, GUID, PWSTR, VARIANT};
use windows::Win32::Graphics::Direct3D11::{ID3D11Texture2D, D3D11_BIND_RENDER_TARGET, D3D11_USAGE_DEFAULT};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::CoTaskMemFree;

use crate::config::{Codec, Preset, RateControl};
use crate::gpu::Gpu;

pub struct EncodedFrame {
    pub data: Vec<u8>,
    pub pts: i64,
    pub key: bool,
}

#[derive(Clone, Debug)]
pub struct EncoderOption {
    pub codec: Codec,
    pub name: String,
    #[allow(dead_code)]
    pub hardware: bool,
}

pub struct VideoSettings {
    pub codec: Codec,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub rate_control: RateControl,
    pub bitrate_mbps: u32,
    pub quality: u32,
    pub preset: Preset,
    /// Encoder low-latency mode (worse compression; recording does not need it).
    pub low_latency: bool,
    /// Keyframe interval in seconds.
    pub gop_secs: u32,
    /// Peak bitrate cap for the constant-quality mode (0 = none).
    pub peak_mbps: u32,
}

fn subtype(c: Codec) -> GUID {
    match c {
        Codec::H264 => MFVideoFormat_H264,
        Codec::Hevc => MFVideoFormat_HEVC,
        Codec::Av1 => MFVideoFormat_AV1,
    }
}

unsafe fn enum_activates(codec: Codec, hardware: bool) -> Vec<IMFActivate> {
    let input = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: MFVideoFormat_NV12 };
    let output = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: subtype(codec) };
    let flags = if hardware {
        MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER
    } else {
        MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_SORTANDFILTER
    };
    let mut pp: *mut Option<IMFActivate> = std::ptr::null_mut();
    let mut n = 0u32;
    if MFTEnumEx(MFT_CATEGORY_VIDEO_ENCODER, flags, Some(&input), Some(&output), &mut pp, &mut n).is_err()
        || pp.is_null()
    {
        return Vec::new();
    }
    let slice = std::slice::from_raw_parts_mut(pp, n as usize);
    let list: Vec<IMFActivate> = slice.iter_mut().filter_map(|a| a.take()).collect();
    CoTaskMemFree(Some(pp as *const c_void));
    list
}

unsafe fn activate_name(a: &IMFActivate) -> String {
    let mut p = PWSTR::null();
    let mut len = 0u32;
    if a.GetAllocatedString(&MFT_FRIENDLY_NAME_Attribute, &mut p, &mut len).is_ok() && !p.is_null() {
        let s = p.to_string().unwrap_or_default();
        CoTaskMemFree(Some(p.0 as *const c_void));
        s
    } else {
        String::from("Encoder")
    }
}

unsafe fn activate_luid(a: &IMFActivate) -> Option<(u32, i32)> {
    let mut buf = [0u8; 8];
    let mut size = 0u32;
    a.GetBlob(&MFT_ENUM_ADAPTER_LUID, &mut buf, Some(&mut size)).ok()?;
    Some((
        u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]),
        i32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]),
    ))
}

/// Ordered candidates for a codec: hardware encoders on our adapter first.
unsafe fn candidates(codec: Codec, luid: (u32, i32)) -> Vec<(IMFActivate, bool)> {
    let mut hw = enum_activates(codec, true);
    hw.sort_by_key(|a| if activate_luid(a) == Some(luid) { 0 } else { 1 });
    let mut out: Vec<(IMFActivate, bool)> = hw.into_iter().map(|a| (a, true)).collect();
    if codec == Codec::H264 {
        out.extend(enum_activates(codec, false).into_iter().map(|a| (a, false)));
    }
    out
}

/// Lists usable encoders per codec (for the settings UI).
pub fn probe(gpu: &Gpu) -> Vec<EncoderOption> {
    let mut out = Vec::new();
    unsafe {
        for codec in Codec::ALL {
            if let Some((a, hw)) = candidates(codec, gpu.luid).into_iter().next() {
                out.push(EncoderOption { codec, name: activate_name(&a), hardware: hw });
            }
        }
    }
    out
}

pub struct VideoEncoder {
    mft: IMFTransform,
    events: Option<IMFMediaEventGenerator>,
    codec_api: Option<ICodecAPI>,
    in_id: u32,
    out_id: u32,
    provides_samples: bool,
    out_size: u32,
    need_input: u32,
    allocator: IMFVideoSampleAllocatorEx,
    _manager: IMFDXGIDeviceManager,
    frame_dur: i64,
    pub name: String,
    #[allow(dead_code)]
    pub hardware: bool,
}

unsafe fn set_api(api: &ICodecAPI, key: &GUID, v: VARIANT, what: &str) {
    if let Err(e) = api.SetValue(key, &v) {
        log::info!("encoder ignores {what}: {e}");
    }
}

impl VideoEncoder {
    pub fn new(gpu: &Gpu, s: &VideoSettings) -> windows::core::Result<Self> {
        unsafe {
            let mut last_err = windows::core::Error::from(MF_E_TOPO_CODEC_NOT_FOUND);
            for (activate, hw) in candidates(s.codec, gpu.luid) {
                let name = activate_name(&activate);
                match Self::open(gpu, s, &activate, hw, name.clone()) {
                    Ok(e) => {
                        log::info!("video encoder: {name} ({}x{} @ {} fps, {:?})", s.width, s.height, s.fps, s.codec);
                        return Ok(e);
                    }
                    Err(e) => {
                        log::warn!("encoder {name} failed: {e}");
                        let _ = activate.ShutdownObject();
                        last_err = e;
                    }
                }
            }
            Err(last_err)
        }
    }

    unsafe fn open(
        gpu: &Gpu,
        s: &VideoSettings,
        activate: &IMFActivate,
        hardware: bool,
        name: String,
    ) -> windows::core::Result<Self> {
        let mft: IMFTransform = activate.ActivateObject()?;
        let attrs = mft.GetAttributes().ok();
        let is_async = attrs.as_ref().and_then(|a| a.GetUINT32(&MF_TRANSFORM_ASYNC).ok()).unwrap_or(0) != 0;
        if let Some(a) = &attrs {
            if is_async {
                a.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1)?;
            }
            let _ = a.SetUINT32(&MF_LOW_LATENCY, s.low_latency as u32);
        }

        let mut token = 0u32;
        let mut manager: Option<IMFDXGIDeviceManager> = None;
        MFCreateDXGIDeviceManager(&mut token, &mut manager)?;
        let manager = manager.unwrap();
        manager.ResetDevice(&gpu.device, token)?;
        if hardware {
            mft.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, manager.as_raw() as usize)?;
        }

        let codec_api: Option<ICodecAPI> = mft.cast().ok();
        let gop = (s.fps * s.gop_secs.max(1)).max(1);
        if let Some(api) = &codec_api {
            Self::configure(api, s, gop);
        }

        // Output type first (required by encoders), then input.
        let out_t = MFCreateMediaType()?;
        out_t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        out_t.SetGUID(&MF_MT_SUBTYPE, &subtype(s.codec))?;
        out_t.SetUINT32(&MF_MT_AVG_BITRATE, s.bitrate_mbps * 1_000_000)?;
        out_t.SetUINT64(&MF_MT_FRAME_SIZE, ((s.width as u64) << 32) | s.height as u64)?;
        out_t.SetUINT64(&MF_MT_FRAME_RATE, ((s.fps as u64) << 32) | 1)?;
        out_t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1u64 << 32) | 1)?;
        out_t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        match s.codec {
            Codec::H264 => {
                out_t.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_High.0 as u32)?;
            }
            Codec::Hevc => {
                out_t.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH265VProfile_Main_420_8.0 as u32)?;
            }
            Codec::Av1 => {}
        }
        Self::color_attrs(&out_t);
        mft.SetOutputType(0, &out_t, 0)?;

        let mut in_t: Option<IMFMediaType> = None;
        for i in 0..32 {
            match mft.GetInputAvailableType(0, i) {
                Ok(t) => {
                    if t.GetGUID(&MF_MT_SUBTYPE).ok() == Some(MFVideoFormat_NV12) {
                        in_t = Some(t);
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        let in_t = match in_t {
            Some(t) => t,
            None => {
                let t = MFCreateMediaType()?;
                t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
                t.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
                t
            }
        };
        in_t.SetUINT64(&MF_MT_FRAME_SIZE, ((s.width as u64) << 32) | s.height as u64)?;
        in_t.SetUINT64(&MF_MT_FRAME_RATE, ((s.fps as u64) << 32) | 1)?;
        in_t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1u64 << 32) | 1)?;
        in_t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        Self::color_attrs(&in_t);
        mft.SetInputType(0, &in_t, 0)?;

        // Some encoders only accept rate-control changes after types are set: apply again.
        if let Some(api) = &codec_api {
            Self::configure(api, s, gop);
        }

        let mut in_id = 0u32;
        let mut out_id = 0u32;
        if mft.GetStreamIDs(std::slice::from_mut(&mut in_id), std::slice::from_mut(&mut out_id)).is_err() {
            in_id = 0;
            out_id = 0;
        }
        let info = mft.GetOutputStreamInfo(out_id)?;
        let provides_samples = info.dwFlags & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0 as u32) != 0;

        // Sample allocator: NV12 render-target textures recycled when the encoder releases them.
        let mut alloc_raw: *mut c_void = std::ptr::null_mut();
        MFCreateVideoSampleAllocatorEx(&IMFVideoSampleAllocatorEx::IID, &mut alloc_raw)?;
        let allocator = IMFVideoSampleAllocatorEx::from_raw(alloc_raw);
        allocator.SetDirectXManager(&manager)?;
        let mut aattrs: Option<IMFAttributes> = None;
        MFCreateAttributes(&mut aattrs, 3)?;
        let aattrs = aattrs.unwrap();
        aattrs.SetUINT32(&MF_SA_D3D11_BINDFLAGS, D3D11_BIND_RENDER_TARGET.0 as u32)?;
        aattrs.SetUINT32(&MF_SA_D3D11_USAGE, D3D11_USAGE_DEFAULT.0 as u32)?;
        allocator.InitializeSampleAllocatorEx(4, 24, &aattrs, &in_t)?;

        let events: Option<IMFMediaEventGenerator> = if is_async { Some(mft.cast()?) } else { None };

        mft.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
        mft.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;

        Ok(VideoEncoder {
            mft,
            events,
            codec_api,
            in_id,
            out_id,
            provides_samples,
            out_size: info.cbSize.max(s.width * s.height * 2),
            need_input: if is_async { 0 } else { u32::MAX },
            allocator,
            _manager: manager,
            frame_dur: crate::util::HNS / s.fps.max(1) as i64,
            name,
            hardware,
        })
    }

    unsafe fn color_attrs(t: &IMFMediaType) {
        let _ = t.SetUINT32(&MF_MT_VIDEO_PRIMARIES, MFVideoPrimaries_BT709.0 as u32);
        let _ = t.SetUINT32(&MF_MT_TRANSFER_FUNCTION, MFVideoTransFunc_709.0 as u32);
        let _ = t.SetUINT32(&MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT709.0 as u32);
        let _ = t.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32);
    }

    unsafe fn configure(api: &ICodecAPI, s: &VideoSettings, gop: u32) {
        let bitrate = s.bitrate_mbps * 1_000_000;
        match s.rate_control {
            RateControl::Cbr => {
                set_api(api, &CODECAPI_AVEncCommonRateControlMode, VARIANT::from(eAVEncCommonRateControlMode_CBR.0 as u32), "rc");
                set_api(api, &CODECAPI_AVEncCommonMeanBitRate, VARIANT::from(bitrate), "mean");
            }
            RateControl::Vbr => {
                set_api(api, &CODECAPI_AVEncCommonRateControlMode, VARIANT::from(eAVEncCommonRateControlMode_PeakConstrainedVBR.0 as u32), "rc");
                set_api(api, &CODECAPI_AVEncCommonMeanBitRate, VARIANT::from(bitrate), "mean");
                set_api(api, &CODECAPI_AVEncCommonMaxBitRate, VARIANT::from(bitrate / 2 * 3), "max");
            }
            RateControl::Cqp => {
                set_api(api, &CODECAPI_AVEncCommonRateControlMode, VARIANT::from(eAVEncCommonRateControlMode_Quality.0 as u32), "rc");
                set_api(api, &CODECAPI_AVEncCommonQuality, VARIANT::from(s.quality.clamp(1, 100)), "quality");
                // QP: quality 100 → 12, quality 1 → 45 (H.264/HEVC scale; AV1 MFTs map it too).
                let qp = (45.0 - (s.quality.clamp(1, 100) as f64 - 1.0) / 99.0 * 33.0).round() as u64;
                set_api(api, &CODECAPI_AVEncVideoEncodeQP, VARIANT::from(qp), "qp");
                if s.peak_mbps > 0 {
                    set_api(api, &CODECAPI_AVEncCommonMaxBitRate, VARIANT::from(s.peak_mbps * 1_000_000), "max");
                }
            }
        }
        let speed = match s.preset {
            Preset::Speed => 0u32,
            Preset::Balanced => 50,
            Preset::Quality => 100,
        };
        set_api(api, &CODECAPI_AVEncCommonQualityVsSpeed, VARIANT::from(speed), "preset");
        set_api(api, &CODECAPI_AVEncMPVGOPSize, VARIANT::from(gop), "gop");
        set_api(api, &CODECAPI_AVEncMPVDefaultBPictureCount, VARIANT::from(0u32), "bframes");
        set_api(api, &CODECAPI_AVLowLatencyMode, VARIANT::from(s.low_latency), "lowlatency");
    }

    /// Gets a free NV12 surface from the pool. `None` when the encoder still holds them all.
    pub fn alloc_surface(&self) -> windows::core::Result<Option<(IMFSample, ID3D11Texture2D, u32)>> {
        unsafe {
            let sample = match self.allocator.AllocateSample() {
                Ok(s) => s,
                Err(e) if e.code() == MF_E_SAMPLEALLOCATOR_EMPTY => return Ok(None),
                Err(e) => return Err(e),
            };
            let buf = sample.GetBufferByIndex(0)?;
            let dxgi: IMFDXGIBuffer = buf.cast()?;
            let mut raw: *mut c_void = std::ptr::null_mut();
            dxgi.GetResource(&ID3D11Texture2D::IID, &mut raw)?;
            let tex = ID3D11Texture2D::from_raw(raw);
            let sub = dxgi.GetSubresourceIndex().unwrap_or(0);
            if let Ok(len) = buf.GetMaxLength() {
                let _ = buf.SetCurrentLength(len);
            }
            Ok(Some((sample, tex, sub)))
        }
    }

    /// True when the encoder is ready to accept another frame.
    pub fn can_accept(&self) -> bool {
        self.need_input > 0
    }

    pub fn force_keyframe(&self) {
        if let Some(api) = &self.codec_api {
            unsafe { set_api(api, &CODECAPI_AVEncVideoForceKeyFrame, VARIANT::from(1u32), "forcekey") };
        }
    }

    /// Submits a frame. Encoded output (if any) is appended to `out`.
    pub fn encode(&mut self, sample: &IMFSample, pts: i64, out: &mut Vec<EncodedFrame>) -> windows::core::Result<()> {
        unsafe {
            sample.SetSampleTime(pts)?;
            sample.SetSampleDuration(self.frame_dur)?;
            if self.events.is_some() {
                self.mft.ProcessInput(self.in_id, sample, 0)?;
                self.need_input = self.need_input.saturating_sub(1);
                self.poll(out)?;
            } else {
                loop {
                    match self.mft.ProcessInput(self.in_id, sample, 0) {
                        Ok(()) => break,
                        Err(e) if e.code() == MF_E_NOTACCEPTING => {
                            if !self.pull_output(out)? {
                                return Err(e);
                            }
                        }
                        Err(e) => return Err(e),
                    }
                }
                while self.pull_output(out)? {}
            }
        }
        Ok(())
    }

    /// Processes pending async events (input requests and finished frames).
    pub fn poll(&mut self, out: &mut Vec<EncodedFrame>) -> windows::core::Result<()> {
        let Some(events) = self.events.clone() else { return Ok(()) };
        unsafe {
            loop {
                let ev = match events.GetEvent(MF_EVENT_FLAG_NO_WAIT) {
                    Ok(ev) => ev,
                    Err(e) if e.code() == MF_E_NO_EVENTS_AVAILABLE => break,
                    Err(e) => return Err(e),
                };
                let t = ev.GetType()?;
                if t == METransformNeedInput.0 as u32 {
                    self.need_input += 1;
                } else if t == METransformHaveOutput.0 as u32 {
                    self.pull_output(out)?;
                }
            }
        }
        Ok(())
    }

    /// Returns `Ok(true)` when a frame was produced, `Ok(false)` when more input is needed.
    unsafe fn pull_output(&mut self, out: &mut Vec<EncodedFrame>) -> windows::core::Result<bool> {
        let sample = if self.provides_samples {
            None
        } else {
            let s = MFCreateSample()?;
            let b = MFCreateMemoryBuffer(self.out_size)?;
            s.AddBuffer(&b)?;
            Some(s)
        };
        let mut buf = [MFT_OUTPUT_DATA_BUFFER {
            dwStreamID: self.out_id,
            pSample: std::mem::ManuallyDrop::new(sample),
            dwStatus: 0,
            pEvents: std::mem::ManuallyDrop::new(None),
        }];
        let mut status = 0u32;
        let r = self.mft.ProcessOutput(0, &mut buf, &mut status);
        let sample = std::mem::ManuallyDrop::take(&mut buf[0].pSample);
        drop(std::mem::ManuallyDrop::take(&mut buf[0].pEvents));
        match r {
            Ok(()) => {}
            Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(false),
            Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                let t = self.mft.GetOutputAvailableType(self.out_id, 0)?;
                self.mft.SetOutputType(self.out_id, &t, 0)?;
                return Ok(true);
            }
            Err(e) => return Err(e),
        }
        let Some(sample) = sample else { return Ok(true) };
        let pts = sample.GetSampleTime().unwrap_or(0);
        let key = sample.GetUINT32(&MFSampleExtension_CleanPoint).unwrap_or(0) != 0;
        let mb = sample.ConvertToContiguousBuffer()?;
        let mut p: *mut u8 = std::ptr::null_mut();
        let mut len = 0u32;
        mb.Lock(&mut p, None, Some(&mut len))?;
        let data = std::slice::from_raw_parts(p, len as usize).to_vec();
        let _ = mb.Unlock();
        if !data.is_empty() {
            out.push(EncodedFrame { data, pts, key });
        }
        Ok(true)
    }
}

impl Drop for VideoEncoder {
    fn drop(&mut self) {
        unsafe {
            let _ = self.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
            let _ = self.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
            let _ = self.mft.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, 0);
            if let Some(ev) = &self.events {
                if let Ok(s) = ev.cast::<IMFShutdown>() {
                    let _ = s.Shutdown();
                }
            }
            let _ = self.allocator.UninitializeSampleAllocator();
        }
    }
}

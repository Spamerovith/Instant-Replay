//! AAC-LC encoding via the Windows Media Foundation AAC encoder (48 kHz stereo).

use windows::core::Result;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};

use crate::media::{AAC_FRAME, AUDIO_RATE};

pub struct AacEncoder {
    mft: IMFTransform,
    out_size: u32,
}

impl AacEncoder {
    pub fn new(bitrate_kbps: u32) -> Result<Self> {
        unsafe {
            let mft: IMFTransform = CoCreateInstance(&AACMFTEncoder, None, CLSCTX_INPROC_SERVER)?;
            let bytes_per_sec = match bitrate_kbps {
                96 => 12000,
                128 => 16000,
                160 => 20000,
                _ => 24000,
            };
            let in_t = MFCreateMediaType()?;
            in_t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
            in_t.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM)?;
            in_t.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
            in_t.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, AUDIO_RATE)?;
            in_t.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, 2)?;
            in_t.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, 4)?;
            in_t.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, AUDIO_RATE * 4)?;

            let out_t = MFCreateMediaType()?;
            out_t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
            out_t.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_AAC)?;
            out_t.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
            out_t.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, AUDIO_RATE)?;
            out_t.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, 2)?;
            out_t.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, bytes_per_sec)?;
            out_t.SetUINT32(&MF_MT_AAC_PAYLOAD_TYPE, 0)?; // raw AAC frames
            out_t.SetUINT32(&MF_MT_AAC_AUDIO_PROFILE_LEVEL_INDICATION, 0x29)?;

            if mft.SetInputType(0, &in_t, 0).is_ok() {
                mft.SetOutputType(0, &out_t, 0)?;
            } else {
                mft.SetOutputType(0, &out_t, 0)?;
                mft.SetInputType(0, &in_t, 0)?;
            }
            let info = mft.GetOutputStreamInfo(0)?;
            mft.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            mft.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
            Ok(AacEncoder { mft, out_size: info.cbSize.max(8192) })
        }
    }

    /// Encodes exactly one AAC frame worth of interleaved stereo samples (1024 × 2).
    /// Returns finished AAC frames (raw, without ADTS).
    pub fn encode(&mut self, pcm: &[i16], pts: i64, out: &mut Vec<Vec<u8>>) -> Result<()> {
        unsafe {
            let bytes = pcm.len() * 2;
            let buf = MFCreateMemoryBuffer(bytes as u32)?;
            let mut p: *mut u8 = std::ptr::null_mut();
            buf.Lock(&mut p, None, None)?;
            std::ptr::copy_nonoverlapping(pcm.as_ptr() as *const u8, p, bytes);
            buf.Unlock()?;
            buf.SetCurrentLength(bytes as u32)?;
            let s = MFCreateSample()?;
            s.AddBuffer(&buf)?;
            s.SetSampleTime(pts)?;
            s.SetSampleDuration(AAC_FRAME * crate::util::HNS / AUDIO_RATE as i64)?;
            loop {
                match self.mft.ProcessInput(0, &s, 0) {
                    Ok(()) => break,
                    Err(e) if e.code() == MF_E_NOTACCEPTING => {
                        if !self.pull(out)? {
                            return Err(e);
                        }
                    }
                    Err(e) => return Err(e),
                }
            }
            while self.pull(out)? {}
        }
        Ok(())
    }

    unsafe fn pull(&mut self, out: &mut Vec<Vec<u8>>) -> Result<bool> {
        let s = MFCreateSample()?;
        let b = MFCreateMemoryBuffer(self.out_size)?;
        s.AddBuffer(&b)?;
        let mut buf = [MFT_OUTPUT_DATA_BUFFER {
            dwStreamID: 0,
            pSample: std::mem::ManuallyDrop::new(Some(s)),
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
            Err(e) => return Err(e),
        }
        let Some(sample) = sample else { return Ok(false) };
        let mb = sample.ConvertToContiguousBuffer()?;
        let mut p: *mut u8 = std::ptr::null_mut();
        let mut len = 0u32;
        mb.Lock(&mut p, None, Some(&mut len))?;
        if len > 0 {
            out.push(std::slice::from_raw_parts(p, len as usize).to_vec());
        }
        let _ = mb.Unlock();
        Ok(true)
    }
}

impl Drop for AacEncoder {
    fn drop(&mut self) {
        unsafe {
            let _ = self.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
        }
    }
}

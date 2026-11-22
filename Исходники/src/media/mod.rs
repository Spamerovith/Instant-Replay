//! Codec-independent media types shared by encoders, the replay buffer and the muxer.

pub mod bitstream;
pub mod mp4;

use std::sync::Arc;

use crate::config::Codec;

/// One encoded access unit. Timestamps are in 100-ns units relative to the session start.
#[derive(Clone)]
pub struct Packet {
    pub pts: i64,
    pub key: bool,
    pub data: Arc<[u8]>,
}

#[derive(Clone, Debug)]
pub struct VideoInfo {
    pub codec: Codec,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// Payload of the avcC / hvcC / av1C configuration box.
    pub config: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct AudioInfo {
    pub sample_rate: u32,
    pub channels: u16,
    pub bitrate: u32,
    /// AudioSpecificConfig.
    pub asc: Vec<u8>,
    pub name: &'static str,
}

/// AAC frames are always 1024 samples.
pub const AAC_FRAME: i64 = 1024;
pub const AUDIO_RATE: u32 = 48_000;

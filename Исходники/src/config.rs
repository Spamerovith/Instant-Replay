//! Persistent user settings (`%APPDATA%\InstantReplay\config.json`).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::util;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Codec {
    H264,
    Hevc,
    Av1,
}

impl Codec {
    pub const ALL: [Codec; 3] = [Codec::H264, Codec::Hevc, Codec::Av1];
    pub fn label(self) -> &'static str {
        match self {
            Codec::H264 => "H.264",
            Codec::Hevc => "HEVC",
            Codec::Av1 => "AV1",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum RateControl {
    Cbr,
    Vbr,
    Cqp,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum Preset {
    Speed,
    Balanced,
    Quality,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum Resolution {
    Native,
    P2160,
    P1440,
    P1080,
    P720,
}

impl Resolution {
    pub const ALL: [Resolution; 5] =
        [Resolution::Native, Resolution::P2160, Resolution::P1440, Resolution::P1080, Resolution::P720];
    pub fn label(self) -> &'static str {
        match self {
            Resolution::Native => tr!("Как у источника", "Same as source", "Як у джерела"),
            Resolution::P2160 => "2160p (4K)",
            Resolution::P1440 => "1440p (2K)",
            Resolution::P1080 => "1080p",
            Resolution::P720 => "720p",
        }
    }
    pub fn height(self) -> Option<u32> {
        match self {
            Resolution::Native => None,
            Resolution::P2160 => Some(2160),
            Resolution::P1440 => Some(1440),
            Resolution::P1080 => Some(1080),
            Resolution::P720 => Some(720),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub enum CaptureSource {
    /// GDI device name of the monitor, e.g. `\\.\DISPLAY1`. Empty = primary.
    Monitor { device: String },
    /// Top-level window, matched by executable name first and title second.
    Window { title: String, exe: String },
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Hotkey {
    pub vk: u32,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
}

impl Hotkey {
    pub const fn new(vk: u32, ctrl: bool, alt: bool, shift: bool) -> Self {
        Hotkey { vk, ctrl, alt, shift, win: false }
    }
    pub fn is_set(&self) -> bool {
        self.vk != 0
    }
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(default)]
pub struct Config {
    pub replay_enabled: bool,
    pub replay_seconds: u32,

    pub source: CaptureSource,
    pub capture_cursor: bool,

    pub codec: Codec,
    pub rate_control: RateControl,
    pub bitrate_mbps: u32,
    /// Bitrate follows the picture size, frame rate and codec; `bitrate_mbps` is used when off.
    pub auto_bitrate: bool,
    /// 1..=100, higher is better. Used in CQP mode.
    pub quality: u32,
    pub preset: Preset,
    pub fps: u32,
    pub resolution: Resolution,

    pub system_audio: bool,
    pub system_volume: f32,
    pub mic_enabled: bool,
    pub mic_volume: f32,
    /// Endpoint id of the microphone; `None` = default communications/console device.
    pub mic_device: Option<String>,
    pub separate_tracks: bool,
    pub audio_bitrate_kbps: u32,

    pub hotkey_save_replay: Hotkey,
    /// Saves only the last `save_last_seconds` of the buffer.
    pub hotkey_save_last: Hotkey,
    pub save_last_seconds: u32,
    pub hotkey_record: Hotkey,
    pub hotkey_toggle_replay: Hotkey,
    pub hotkey_overlay: Hotkey,

    pub output_dir: PathBuf,
    pub autostart: bool,
    pub start_minimized: bool,
    pub close_to_tray: bool,
    pub notify_overlay: bool,
    pub notify_sound: bool,

    /// Interface language ("ru" / "en" / "uk"); `None` until the first-run choice.
    pub language: Option<String>,
    /// Release the user chose to skip.
    pub skip_version: String,
}

pub const VK_F9: u32 = 0x78;
pub const VK_F10: u32 = 0x79;
pub const VK_F8: u32 = 0x77;
pub const VK_F7: u32 = 0x76;

impl Default for Config {
    fn default() -> Self {
        Config {
            replay_enabled: true,
            replay_seconds: 120,
            source: CaptureSource::Monitor { device: String::new() },
            capture_cursor: true,
            codec: Codec::H264,
            rate_control: RateControl::Vbr,
            bitrate_mbps: 50,
            auto_bitrate: true,
            quality: 80,
            preset: Preset::Balanced,
            fps: 60,
            resolution: Resolution::Native,
            system_audio: true,
            system_volume: 1.0,
            mic_enabled: false,
            mic_volume: 1.0,
            mic_device: None,
            separate_tracks: true,
            audio_bitrate_kbps: 192,
            // Alt+F9/F10 are held by the NVIDIA App overlay on most GeForce systems.
            hotkey_save_replay: Hotkey::new(VK_F10, true, true, false),
            hotkey_save_last: Hotkey::new(VK_F7, true, true, false),
            save_last_seconds: 30,
            hotkey_record: Hotkey::new(VK_F9, true, true, false),
            hotkey_toggle_replay: Hotkey::new(VK_F8, true, true, false),
            hotkey_overlay: Hotkey::new(0x5A, true, true, false), // Ctrl+Alt+Z
            output_dir: util::default_videos_dir(),
            autostart: false,
            start_minimized: false,
            close_to_tray: true,
            notify_overlay: true,
            notify_sound: true,
            language: None,
            skip_version: String::new(),
        }
    }
}

impl Config {
    fn path() -> PathBuf {
        util::app_data_dir().join("config.json")
    }

    pub fn load() -> Config {
        let mut c: Config = std::fs::read_to_string(Self::path())
            .ok()
            .and_then(|s| serde_json::from_str(s.trim_start_matches('\u{feff}')).ok())
            .unwrap_or_default();
        c.sanitize();
        c
    }

    pub fn save(&self) {
        let path = Self::path();
        let tmp = path.with_extension("json.tmp");
        if let Ok(s) = serde_json::to_string_pretty(self) {
            if std::fs::write(&tmp, s).is_ok() {
                let _ = std::fs::rename(&tmp, &path);
            }
        }
    }

    pub fn sanitize(&mut self) {
        self.replay_seconds = self.replay_seconds.clamp(15, 20 * 60);
        self.save_last_seconds = self.save_last_seconds.clamp(5, 20 * 60);
        self.bitrate_mbps = self.bitrate_mbps.clamp(2, 200);
        self.quality = self.quality.clamp(1, 100);
        self.fps = self.fps.clamp(15, 240);
        self.system_volume = self.system_volume.clamp(0.0, 2.0);
        self.mic_volume = self.mic_volume.clamp(0.0, 2.0);
        if ![96, 128, 160, 192].contains(&self.audio_bitrate_kbps) {
            self.audio_bitrate_kbps = 192;
        }
        if self.output_dir.as_os_str().is_empty() {
            self.output_dir = util::default_videos_dir();
        }
    }

    /// Settings that require rebuilding the capture/encode pipeline when changed.
    pub fn pipeline_key(&self) -> PipelineKey {
        PipelineKey {
            source: self.source.clone(),
            capture_cursor: self.capture_cursor,
            codec: self.codec,
            rate_control: self.rate_control,
            bitrate_mbps: self.bitrate_mbps,
            auto_bitrate: self.auto_bitrate,
            quality: self.quality,
            preset: self.preset,
            fps: self.fps,
            resolution: self.resolution,
            system_audio: self.system_audio,
            mic_enabled: self.mic_enabled,
            mic_device: self.mic_device.clone(),
            separate_tracks: self.separate_tracks,
            audio_bitrate_kbps: self.audio_bitrate_kbps,
        }
    }
}

#[derive(Clone, PartialEq, Debug)]
pub struct PipelineKey {
    pub source: CaptureSource,
    pub capture_cursor: bool,
    pub codec: Codec,
    pub rate_control: RateControl,
    pub bitrate_mbps: u32,
    /// Bitrate follows the picture size, frame rate and codec; `bitrate_mbps` is used when off.
    pub auto_bitrate: bool,
    pub quality: u32,
    pub preset: Preset,
    pub fps: u32,
    pub resolution: Resolution,
    pub system_audio: bool,
    pub mic_enabled: bool,
    pub mic_device: Option<String>,
    pub separate_tracks: bool,
    pub audio_bitrate_kbps: u32,
}

/// Bitrate (Mbps) that keeps a clip sharp at the given picture size and frame rate. Modern codecs
/// need fewer bits per pixel for the same look: AV1 about half of H.264, HEVC about two thirds.
pub fn auto_bitrate_mbps(codec: Codec, width: u32, height: u32, fps: u32) -> u32 {
    let bits_per_pixel = match codec {
        Codec::Av1 => 0.055,
        Codec::Hevc => 0.08,
        Codec::H264 => 0.12,
    };
    let mbps = width as f64 * height as f64 * fps as f64 * bits_per_pixel / 1_000_000.0;
    (mbps.round() as u32).clamp(3, 120)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_bitrate_scales_with_codec_and_size() {
        let av1 = auto_bitrate_mbps(Codec::Av1, 1920, 1080, 60);
        let hevc = auto_bitrate_mbps(Codec::Hevc, 1920, 1080, 60);
        let h264 = auto_bitrate_mbps(Codec::H264, 1920, 1080, 60);
        assert!(av1 < hevc && hevc < h264, "{av1} {hevc} {h264}");
        assert!(av1 <= 8, "1080p60 AV1 must stay light, got {av1}");
        assert!(auto_bitrate_mbps(Codec::Av1, 3840, 2160, 60) > av1 * 3);
        assert_eq!(auto_bitrate_mbps(Codec::Av1, 640, 360, 15), 3);
        assert_eq!(auto_bitrate_mbps(Codec::H264, 7680, 4320, 240), 120);
    }

    #[test]
    fn old_config_without_new_fields_still_loads() {
        let c: Config = serde_json::from_str(r#"{"replay_seconds":60,"bitrate_mbps":50}"#).unwrap();
        assert!(c.auto_bitrate);
        assert_eq!(c.save_last_seconds, 30);
        assert_eq!(c.hotkey_save_last.vk, VK_F7);
    }
}

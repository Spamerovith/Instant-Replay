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
    /// Sharpness of the auto bitrate.
    pub bitrate_quality: BitrateQuality,
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
    pub hotkey_screenshot: Hotkey,
    pub hotkey_hud: Hotkey,

    pub output_dir: PathBuf,
    pub autostart: bool,
    pub start_minimized: bool,
    pub close_to_tray: bool,
    pub notify_overlay: bool,
    pub notify_sound: bool,
    /// Clips go into a folder named after the game (like the NVIDIA overlay does).
    pub game_folders: bool,
    /// The overlay shows up in screen sharing (Discord, OBS) and screenshots. It then also gets
    /// into a recording of the whole monitor.
    pub overlay_visible_in_capture: bool,

    /// Performance counter over the game (FPS, GPU, CPU...).
    pub hud_enabled: bool,
    /// 0 top left, 1 top right, 2 bottom left, 3 bottom right.
    pub hud_corner: u8,
    pub hud_fps: bool,
    /// Smart input-to-screen latency.
    pub hud_latency: bool,
    pub hud_display_latency: bool,
    pub hud_ping: bool,
    pub hud_frame_time: bool,
    pub hud_gpu: bool,
    pub hud_cpu: bool,
    pub hud_ram: bool,
    pub hud_clock: bool,
    /// The counter gets into recordings, screenshots and screen sharing.
    pub hud_in_capture: bool,

    /// Interface language ("ru" / "en" / "uk"); `None` until the first-run choice.
    pub language: Option<String>,
    /// Release the user chose to skip.
    pub skip_version: String,
    /// Per-game settings: the game window becomes the source while the game runs.
    pub profiles: Vec<GameProfile>,
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
            bitrate_quality: BitrateQuality::Standard,
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
            hotkey_screenshot: Hotkey::new(0x70, false, true, false), // Alt+F1
            hotkey_hud: Hotkey::new(0x52, false, true, false), // Alt+R
            output_dir: util::default_videos_dir(),
            autostart: false,
            start_minimized: false,
            close_to_tray: true,
            notify_overlay: true,
            notify_sound: true,
            game_folders: true,
            overlay_visible_in_capture: true,
            hud_enabled: false,
            hud_corner: 1,
            hud_fps: true,
            hud_latency: true,
            hud_display_latency: false,
            hud_ping: false,
            hud_frame_time: false,
            hud_gpu: true,
            hud_cpu: true,
            hud_ram: false,
            hud_clock: false,
            hud_in_capture: false,
            language: None,
            skip_version: String::new(),
            profiles: Vec::new(),
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
            bitrate_quality: self.bitrate_quality,
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
    /// Sharpness of the auto bitrate.
    pub bitrate_quality: BitrateQuality,
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
pub fn auto_bitrate_mbps(codec: Codec, width: u32, height: u32, fps: u32, quality: BitrateQuality) -> u32 {
    let bits_per_pixel = match codec {
        Codec::Av1 => 0.055,
        Codec::Hevc => 0.08,
        Codec::H264 => 0.12,
    };
    let mbps = width as f64 * height as f64 * fps as f64 * bits_per_pixel * quality.factor() / 1_000_000.0;
    (mbps.round() as u32).clamp(3, 120)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_bitrate_scales_with_codec_and_size() {
        let av1 = auto_bitrate_mbps(Codec::Av1, 1920, 1080, 60, BitrateQuality::Standard);
        let hevc = auto_bitrate_mbps(Codec::Hevc, 1920, 1080, 60, BitrateQuality::Standard);
        let h264 = auto_bitrate_mbps(Codec::H264, 1920, 1080, 60, BitrateQuality::Standard);
        assert!(av1 < hevc && hevc < h264, "{av1} {hevc} {h264}");
        assert!(av1 <= 8, "1080p60 AV1 must stay light, got {av1}");
        assert!(auto_bitrate_mbps(Codec::Av1, 3840, 2160, 60, BitrateQuality::Standard) > av1 * 3);
        assert_eq!(auto_bitrate_mbps(Codec::Av1, 640, 360, 15, BitrateQuality::Standard), 3);
        assert_eq!(auto_bitrate_mbps(Codec::H264, 7680, 4320, 240, BitrateQuality::Standard), 120);
    }

    #[test]
    fn old_config_without_new_fields_still_loads() {
        let c: Config = serde_json::from_str(r#"{"replay_seconds":60,"bitrate_mbps":50}"#).unwrap();
        assert!(c.auto_bitrate);
        assert_eq!(c.save_last_seconds, 30);
        assert_eq!(c.hotkey_save_last.vk, VK_F7);
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum BitrateQuality {
    Compact,
    Standard,
    High,
}

impl BitrateQuality {
    pub const ALL: [BitrateQuality; 3] = [BitrateQuality::Compact, BitrateQuality::Standard, BitrateQuality::High];
    /// Multiplier of the auto bitrate.
    pub fn factor(self) -> f64 {
        match self {
            BitrateQuality::Compact => 0.7,
            BitrateQuality::Standard => 1.0,
            BitrateQuality::High => 1.5,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            BitrateQuality::Compact => tr!("Компактно", "Compact", "Компактно"),
            BitrateQuality::Standard => tr!("Стандарт", "Standard", "Стандарт"),
            BitrateQuality::High => tr!("Высокое", "High", "Високе"),
        }
    }
}

/// Settings that replace the general ones while the game `exe` has a window.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(default)]
pub struct GameProfile {
    /// Executable name, e.g. `game.exe` (compared without case).
    pub exe: String,
    pub name: String,
    pub fps: u32,
    pub resolution: Resolution,
    pub quality: BitrateQuality,
    pub replay_seconds: u32,
}

impl Default for GameProfile {
    fn default() -> Self {
        GameProfile { exe: String::new(), name: String::new(), fps: 60, resolution: Resolution::Native, quality: BitrateQuality::Standard, replay_seconds: 120 }
    }
}

impl GameProfile {
    /// A new profile starts from the current general settings.
    pub fn from_config(exe: &str, name: &str, c: &Config) -> Self {
        GameProfile { exe: exe.to_string(), name: name.to_string(), fps: c.fps, resolution: c.resolution, quality: c.bitrate_quality, replay_seconds: c.replay_seconds }
    }
}

impl Config {
    /// The settings in force while `p` is active: its game window as the source plus its overrides.
    pub fn with_profile(&self, p: &GameProfile) -> Config {
        let mut c = self.clone();
        c.source = CaptureSource::Window { title: String::new(), exe: p.exe.clone() };
        c.fps = p.fps;
        c.resolution = p.resolution;
        c.bitrate_quality = p.quality;
        c.auto_bitrate = true;
        c.replay_seconds = p.replay_seconds;
        c.sanitize();
        c
    }
}

/// Index of the first profile whose game is among the running window executables.
pub fn find_profile(profiles: &[GameProfile], running_exes: &[String]) -> Option<usize> {
    profiles.iter().position(|p| !p.exe.is_empty() && running_exes.iter().any(|e| e.eq_ignore_ascii_case(&p.exe)))
}

#[cfg(test)]
mod profile_tests {
    use super::*;

    #[test]
    fn quality_scales_auto_bitrate() {
        let c = auto_bitrate_mbps(Codec::H264, 1920, 1080, 60, BitrateQuality::Compact);
        let s = auto_bitrate_mbps(Codec::H264, 1920, 1080, 60, BitrateQuality::Standard);
        let h = auto_bitrate_mbps(Codec::H264, 1920, 1080, 60, BitrateQuality::High);
        assert!(c < s && s < h, "{c} {s} {h}");
    }

    #[test]
    fn profile_overrides_source_and_settings_without_touching_the_base() {
        let base = Config::default();
        let p = GameProfile { exe: "game.exe".into(), name: "Game".into(), fps: 144, resolution: Resolution::P1440, quality: BitrateQuality::High, replay_seconds: 300 };
        let c = base.with_profile(&p);
        assert_eq!(c.source, CaptureSource::Window { title: String::new(), exe: "game.exe".into() });
        assert_eq!((c.fps, c.resolution, c.bitrate_quality, c.replay_seconds), (144, Resolution::P1440, BitrateQuality::High, 300));
        assert_eq!(c.output_dir, base.output_dir);
        assert_eq!(base.fps, 60);
    }

    #[test]
    fn profile_is_found_by_running_exe_ignoring_case() {
        let ps = vec![
            GameProfile { exe: "a.exe".into(), ..Default::default() },
            GameProfile { exe: "Game.EXE".into(), ..Default::default() },
            GameProfile::default(),
        ];
        assert_eq!(find_profile(&ps, &["explorer.exe".into(), "game.exe".into()]), Some(1));
        assert_eq!(find_profile(&ps, &["other.exe".into()]), None);
        assert_eq!(find_profile(&ps, &[]), None);
    }

    #[test]
    fn old_config_without_profiles_loads() {
        let c: Config = serde_json::from_str(r#"{"fps":60}"#).unwrap();
        assert!(c.profiles.is_empty());
        assert_eq!(c.bitrate_quality, BitrateQuality::Standard);
    }
}

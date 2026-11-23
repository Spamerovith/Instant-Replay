//! Настройки в %APPDATA%\JustView\settings.ini

use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct Settings {
    pub volume: f32,
    pub muted: bool,
    pub loop_video: bool,
    pub maximized: bool,
    pub width: f32,
    pub height: f32,
    /// Interface language code ("ru" / "en" / "uk"); `None` until the first-run choice.
    pub lang: Option<String>,
    /// Release the user chose to skip.
    pub skip_version: String,
    /// Unix time of the last update check.
    pub update_checked: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Self { volume: 0.8, muted: false, loop_video: false, maximized: false, width: 0.0, height: 0.0, lang: None, skip_version: String::new(), update_checked: 0 }
    }
}

fn file() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("JustView").join("settings.ini"))
}

impl Settings {
    pub fn load() -> Self {
        let mut s = Self::default();
        let Some(f) = file() else { return s };
        let Ok(text) = std::fs::read_to_string(f) else { return s };
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            let v = v.trim();
            match k.trim() {
                "volume" => s.volume = v.parse().unwrap_or(s.volume),
                "muted" => s.muted = v == "1",
                "loop" => s.loop_video = v == "1",
                "maximized" => s.maximized = v == "1",
                "width" => s.width = v.parse().unwrap_or(0.0),
                "height" => s.height = v.parse().unwrap_or(0.0),
                "lang" if !v.is_empty() => s.lang = Some(v.to_string()),
                "skip_version" => s.skip_version = v.to_string(),
                "update_checked" => s.update_checked = v.parse().unwrap_or(0),
                _ => {}
            }
        }
        s.volume = s.volume.clamp(0.0, 1.0);
        s
    }

    pub fn save(&self) {
        let Some(f) = file() else { return };
        if let Some(d) = f.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let b = |x: bool| if x { "1" } else { "0" };
        let text = format!(
            "volume={:.3}\nmuted={}\nloop={}\nmaximized={}\nwidth={:.0}\nheight={:.0}\nlang={}\nskip_version={}\nupdate_checked={}\n",
            self.volume,
            b(self.muted),
            b(self.loop_video),
            b(self.maximized),
            self.width,
            self.height,
            self.lang.as_deref().unwrap_or(""),
            self.skip_version,
            self.update_checked
        );
        let _ = std::fs::write(f, text);
    }
}

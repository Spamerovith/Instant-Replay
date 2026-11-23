//! Поиск ffmpeg/ffprobe и чтение сведений о медиафайле.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::winutil::hidden_command;

fn find_tool(name: &str) -> Option<PathBuf> {
    let exe = format!("{name}.exe");
    if let Ok(me) = std::env::current_exe() {
        if let Some(dir) = me.parent() {
            for c in [dir.join(&exe), dir.join("ffmpeg").join(&exe), dir.join("ffmpeg").join("bin").join(&exe)] {
                if c.is_file() {
                    return Some(c);
                }
            }
        }
    }
    if let Some(paths) = std::env::var_os("PATH") {
        for d in std::env::split_paths(&paths) {
            let c = d.join(&exe);
            if c.is_file() {
                return Some(c);
            }
        }
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        let c = PathBuf::from(local).join("Microsoft").join("WinGet").join("Links").join(&exe);
        if c.exists() {
            return Some(c);
        }
    }
    for base in [r"C:\ffmpeg\bin", r"C:\Program Files\ffmpeg\bin", r"C:\ProgramData\chocolatey\bin"] {
        let c = Path::new(base).join(&exe);
        if c.is_file() {
            return Some(c);
        }
    }
    None
}

pub fn ffmpeg() -> Option<&'static Path> {
    static P: OnceLock<Option<PathBuf>> = OnceLock::new();
    P.get_or_init(|| find_tool("ffmpeg")).as_deref()
}

pub fn ffprobe() -> Option<&'static Path> {
    static P: OnceLock<Option<PathBuf>> = OnceLock::new();
    P.get_or_init(|| find_tool("ffprobe")).as_deref()
}

pub fn missing() -> &'static str {
    tr!("Для видео, аудио и редких форматов нужен FFmpeg. Положите ffmpeg.exe и ffprobe.exe рядом с JustView.exe или установите: winget install Gyan.FFmpeg", "Video, audio and rare formats need FFmpeg. Put ffmpeg.exe and ffprobe.exe next to JustView.exe or install it: winget install Gyan.FFmpeg", "Для відео, аудіо та рідкісних форматів потрібен FFmpeg. Покладіть ffmpeg.exe і ffprobe.exe поруч із JustView.exe або встановіть: winget install Gyan.FFmpeg")
}

#[derive(Clone, Debug, Default)]
pub struct MediaInfo {
    pub duration: f64,
    pub has_video: bool,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub video_codec: String,
    pub has_audio: bool,
    pub audio_codec: String,
    pub sample_rate: u32,
    pub channels: u32,
    pub has_cover: bool,
    pub bitrate: u64,
    pub format: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    /// Все звуковые дорожки файла (порядок = индекс для `-map 0:a:N`).
    pub audio_tracks: Vec<AudioTrack>,
    /// Дорожка, которую файл просит играть по умолчанию.
    pub default_audio: usize,
}

#[allow(dead_code)]
#[derive(Clone, Debug, Default)]
pub struct AudioTrack {
    /// Title or meaningful handler name from the file (may be empty).
    pub raw: String,
    /// Language tag from the file (may be empty).
    pub lang: String,
    pub index: usize,
    pub codec: String,
    pub channels: u32,
}

impl AudioTrack {
    /// Name shown to the user, in the current interface language.
    pub fn label(&self) -> String {
        match self.raw.to_ascii_lowercase().as_str() {
            "mix" | "main mix" => return tr!("Общий звук (игра + микрофон)", "Combined (game + mic)", "Спільний звук (гра + мікрофон)").to_string(),
            "system audio" | "system" | "desktop audio" => return tr!("Звук системы", "System audio", "Звук системи").to_string(),
            "microphone" | "mic" => return tr!("Микрофон", "Microphone", "Мікрофон").to_string(),
            "" => {}
            _ => return self.raw.clone(),
        }
        if !self.lang.is_empty() && self.lang != "und" {
            return trf!(["Дорожка {} · {}", "Track {} · {}", "Доріжка {} · {}"], self.index + 1, self.lang);
        }
        trf!(["Дорожка {}", "Track {}", "Доріжка {}"], self.index + 1)
    }
}

/// Title → handler_name (when not a generic one) and the language tag.
fn track_tags(s: &serde_json::Value) -> (String, String) {
    let tags = &s["tags"];
    let get = |k: &str| -> String {
        tags.as_object()
            .and_then(|o| o.iter().find(|(kk, _)| kk.eq_ignore_ascii_case(k)).map(|(_, v)| v.as_str().unwrap_or("").trim().to_string()))
            .unwrap_or_default()
    };
    let generic = |h: &str| {
        let l = h.to_ascii_lowercase();
        l.is_empty() || l.contains("soundhandler") || l.contains("sound media handler") || l.contains("core media") || l == "audio" || l.starts_with("isomedia")
    };
    let mut name = get("title");
    if name.is_empty() {
        let h = get("handler_name");
        if !generic(&h) {
            name = h;
        }
    }
    (name, get("language"))
}

fn parse_rate(s: &str) -> f64 {
    if let Some((a, b)) = s.split_once('/') {
        let a: f64 = a.parse().unwrap_or(0.0);
        let b: f64 = b.parse().unwrap_or(1.0);
        if b > 0.0 { a / b } else { 0.0 }
    } else {
        s.parse().unwrap_or(0.0)
    }
}

pub fn probe(path: &Path) -> Result<MediaInfo, String> {
    let probe = ffprobe().ok_or_else(|| missing().to_string())?;
    let out = hidden_command(probe)
        .args(["-v", "error", "-print_format", "json", "-show_format", "-show_streams"])
        .arg(path)
        .output()
        .map_err(|e| e.to_string())?;
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).map_err(|_| tr!("не удалось прочитать файл", "could not read the file", "не вдалося прочитати файл").to_string())?;
    let mut info = MediaInfo::default();
    let fmt = &v["format"];
    info.duration = fmt["duration"].as_str().and_then(|s| s.parse().ok()).unwrap_or(0.0);
    info.bitrate = fmt["bit_rate"].as_str().and_then(|s| s.parse().ok()).unwrap_or(0);
    info.format = fmt["format_long_name"].as_str().unwrap_or("").to_string();
    let tag = |k: &str| -> String {
        let tags = &fmt["tags"];
        if let Some(o) = tags.as_object() {
            for (kk, vv) in o {
                if kk.eq_ignore_ascii_case(k) {
                    return vv.as_str().unwrap_or("").to_string();
                }
            }
        }
        String::new()
    };
    info.title = tag("title");
    info.artist = tag("artist");
    info.album = tag("album");

    let mut default_found = false;
    if let Some(streams) = v["streams"].as_array() {
        for s in streams {
            match s["codec_type"].as_str() {
                Some("video") => {
                    let attached = s["disposition"]["attached_pic"].as_i64() == Some(1);
                    if attached {
                        info.has_cover = true;
                        continue;
                    }
                    if info.has_video {
                        continue;
                    }
                    info.has_video = true;
                    info.width = s["width"].as_u64().unwrap_or(0) as u32;
                    info.height = s["height"].as_u64().unwrap_or(0) as u32;
                    info.video_codec = s["codec_name"].as_str().unwrap_or("").to_string();
                    let mut fps = parse_rate(s["avg_frame_rate"].as_str().unwrap_or("0"));
                    if !(1.0..=240.0).contains(&fps) {
                        fps = parse_rate(s["r_frame_rate"].as_str().unwrap_or("0"));
                    }
                    if !(1.0..=240.0).contains(&fps) {
                        fps = 30.0;
                    }
                    info.fps = fps;
                    // Поворот из метаданных (телефоны): ffmpeg поворачивает кадры сам.
                    let mut rot = 0i64;
                    if let Some(sd) = s["side_data_list"].as_array() {
                        for d in sd {
                            if let Some(r) = d["rotation"].as_i64() {
                                rot = r;
                            }
                        }
                    }
                    if let Some(r) = s["tags"]["rotate"].as_str().and_then(|x| x.parse::<i64>().ok()) {
                        rot = r;
                    }
                    if rot.rem_euclid(180) == 90 {
                        std::mem::swap(&mut info.width, &mut info.height);
                    }
                    if info.duration <= 0.0 {
                        info.duration =
                            s["duration"].as_str().and_then(|x| x.parse().ok()).unwrap_or(0.0);
                    }
                }
                Some("audio") => {
                    let n = info.audio_tracks.len();
                    let codec = s["codec_name"].as_str().unwrap_or("").to_string();
                    let channels = s["channels"].as_u64().unwrap_or(0) as u32;
                    let (raw, lang) = track_tags(s);
                    info.audio_tracks.push(AudioTrack { raw, lang, index: n, codec: codec.clone(), channels });
                    if s["disposition"]["default"].as_i64() == Some(1) && !default_found {
                        info.default_audio = n;
                        default_found = true;
                    }
                    if !info.has_audio {
                        info.has_audio = true;
                        info.audio_codec = codec;
                        info.sample_rate =
                            s["sample_rate"].as_str().and_then(|x| x.parse().ok()).unwrap_or(0);
                        info.channels = channels;
                    }
                }
                _ => {}
            }
        }
    }
    if !info.has_video && !info.has_audio {
        return Err(tr!("в файле нет видео- или аудиопотоков", "the file has no video or audio streams", "у файлі немає відео- чи аудіопотоків").into());
    }
    Ok(info)
}

/// Один кадр/изображение через ffmpeg → RGBA. `stream` — например "0:V:0" или "0:v:0".
pub fn decode_still(path: &Path, map: &str, max_side: u32) -> Result<crate::winutil::Rgba, String> {
    let ff = ffmpeg().ok_or_else(|| missing().to_string())?;
    let vf = format!(
        "scale='min({m},iw)':'min({m},ih)':force_original_aspect_ratio=decrease",
        m = max_side
    );
    let out = hidden_command(ff)
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-i"])
        .arg(path)
        .args(["-map", map, "-frames:v", "1", "-vf", &vf, "-pix_fmt", "rgba", "-c:v", "pam", "-f", "image2pipe", "pipe:1"])
        .output()
        .map_err(|e| e.to_string())?;
    if out.stdout.is_empty() {
        let err = String::from_utf8_lossy(&out.stderr);
        let line = err.lines().last().unwrap_or(tr!("формат не поддерживается", "format not supported", "формат не підтримується")).to_string();
        return Err(line);
    }
    let img = image::load_from_memory_with_format(&out.stdout, image::ImageFormat::Pnm)
        .map_err(|e| e.to_string())?
        .into_rgba8();
    let (w, h) = img.dimensions();
    Ok(crate::winutil::Rgba { w, h, px: img.into_raw() })
}

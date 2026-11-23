//! Декодирование содержимого файла в фоновом потоке.

use std::io::Read;
use std::path::Path;
use std::sync::Arc;

use egui::{Color32, ColorImage};
use image::{AnimationDecoder, DynamicImage, ImageDecoder, ImageFormat, ImageReader};

use crate::ffmpeg::{self, MediaInfo};
use crate::formats::{Kind, ext_of, kind_of};
use crate::winutil::{self, Rgba};

pub struct Frame {
    pub image: Arc<ColorImage>,
    /// Длительность кадра, секунды (для анимаций).
    pub delay: f32,
}

pub enum Content {
    Image {
        frames: Vec<Frame>,
        /// Исходный размер (до уменьшения под текстуру).
        orig: [u32; 2],
        note: Option<String>,
    },
    Media {
        info: MediaInfo,
        cover: Option<Arc<ColorImage>>,
    },
    Pdf {
        pages: u32,
        first: Arc<ColorImage>,
    },
    Text {
        text: String,
        truncated: bool,
    },
    /// Файл не открывается как медиа — показываем превью Проводника.
    Preview {
        image: Option<Arc<ColorImage>>,
    },
    Error(String),
}

pub struct Decoded {
    pub content: Content,
    pub exif: Vec<(String, String)>,
}

fn to_color_image(w: u32, h: u32, rgba: &[u8]) -> ColorImage {
    ColorImage::from_rgba_unmultiplied([w as usize, h as usize], rgba)
}

fn limit(img: DynamicImage, max_side: u32) -> DynamicImage {
    if img.width() > max_side || img.height() > max_side {
        img.resize(max_side, max_side, image::imageops::FilterType::Triangle)
    } else {
        img
    }
}

fn rgba_limit(r: Rgba, max_side: u32) -> (ColorImage, [u32; 2]) {
    let orig = [r.w, r.h];
    if r.w > max_side || r.h > max_side {
        let buf = image::RgbaImage::from_raw(r.w, r.h, r.px).unwrap();
        let img = limit(DynamicImage::ImageRgba8(buf), max_side).into_rgba8();
        (to_color_image(img.width(), img.height(), img.as_raw()), orig)
    } else {
        (to_color_image(r.w, r.h, &r.px), orig)
    }
}

fn single(img: ColorImage, orig: [u32; 2]) -> Content {
    Content::Image {
        frames: vec![Frame { image: Arc::new(img), delay: 0.0 }],
        orig,
        note: None,
    }
}

/// Главная точка входа.
pub fn decode(path: &Path, max_side: u32) -> Decoded {
    let exif = read_exif(path);
    let content = match kind_of(path) {
        Kind::Image => decode_image(path, max_side),
        Kind::Video | Kind::Audio => decode_media(path, max_side),
        Kind::Pdf => match crate::pdf::open(path, 1600) {
            Ok((pages, r)) => {
                let (img, _) = rgba_limit(r, max_side);
                Content::Pdf { pages, first: Arc::new(img) }
            }
            Err(e) => fallback_preview(path, max_side, e),
        },
        Kind::Text => decode_text(path),
        Kind::Other => decode_unknown(path, max_side),
    };
    Decoded { content, exif }
}

fn decode_media(path: &Path, max_side: u32) -> Content {
    match ffmpeg::probe(path) {
        Ok(info) => {
            let cover = if !info.has_video && info.has_cover {
                ffmpeg::decode_still(path, "0:v:0", max_side.min(2048))
                    .ok()
                    .map(|r| Arc::new(rgba_limit(r, max_side).0))
            } else {
                None
            };
            Content::Media { info, cover }
        }
        Err(e) => {
            // Может, это на самом деле изображение (например .gifv/.mjpeg)
            match decode_image(path, max_side) {
                c @ Content::Image { .. } => c,
                _ => Content::Error(e),
            }
        }
    }
}

fn decode_image(path: &Path, max_side: u32) -> Content {
    let ext = ext_of(path);
    if ext == "svg" || ext == "svgz" {
        return match decode_svg(path, max_side) {
            Ok(c) => c,
            Err(e) => fallback_preview(path, max_side, e),
        };
    }
    // 1. Нативные Rust-декодеры (самые быстрые).
    let mut first_err = match decode_with_image_crate(path, max_side) {
        Ok(c) => return c,
        Err(e) => Some(e),
    };
    // 2. Windows Imaging Component (HEIC, RAW, JXR, AVIF при наличии расширений).
    if let Ok((r, orientation)) = winutil::wic_decode(path) {
        let (w, h) = (r.w, r.h);
        let mut img = DynamicImage::ImageRgba8(image::RgbaImage::from_raw(w, h, r.px).unwrap());
        if let Some(o) = image::metadata::Orientation::from_exif(orientation as u8) {
            img.apply_orientation(o);
        }
        let orig = [img.width(), img.height()];
        let img = limit(img, max_side).into_rgba8();
        return single(to_color_image(img.width(), img.height(), img.as_raw()), orig);
    }
    // 3. FFmpeg (AVIF, HEIF, JXL, PSD, EXR, DPX, JPEG 2000 и сотни других).
    if ffmpeg::ffmpeg().is_some() {
        match ffmpeg::decode_still(path, "0:v:0", max_side) {
            Ok(r) => {
                let (img, orig) = rgba_limit(r, max_side);
                return single(img, orig);
            }
            Err(e) => first_err = Some(e),
        }
    }
    fallback_preview(path, max_side, first_err.unwrap_or_else(|| tr!("формат не поддерживается", "format not supported", "формат не підтримується").into()))
}

fn fallback_preview(path: &Path, max_side: u32, err: String) -> Content {
    // 4. Превью оболочки Windows (то, что показывает Проводник).
    if let Some(r) = winutil::shell_thumbnail(path, 1024) {
        if r.w >= 256 {
            let (img, orig) = rgba_limit(r, max_side);
            return Content::Image {
                frames: vec![Frame { image: Arc::new(img), delay: 0.0 }],
                orig,
                note: Some(tr!("Показано превью Windows", "Showing the Windows preview", "Показано мініатюру Windows").into()),
            };
        }
    }
    Content::Error(err)
}

fn decode_with_image_crate(path: &Path, max_side: u32) -> Result<Content, String> {
    let reader = ImageReader::open(path)
        .map_err(|e| e.to_string())?
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    let format = reader.format().ok_or(tr!("неизвестный формат", "unknown format", "невідомий формат"))?;

    // Анимации: GIF, APNG, WebP.
    let animated = match format {
        ImageFormat::Gif => decode_gif(path, max_side),
        ImageFormat::Png => decode_apng(path, max_side),
        ImageFormat::WebP => decode_webp_anim(path, max_side),
        _ => None,
    };
    if let Some(c) = animated {
        return Ok(c);
    }

    let mut dec = reader.into_decoder().map_err(|e| e.to_string())?;
    let orientation = dec.orientation().unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut img = DynamicImage::from_decoder(dec).map_err(|e| e.to_string())?;
    img.apply_orientation(orientation);
    let orig = [img.width(), img.height()];
    let img = limit(img, max_side);
    let rgba = img.into_rgba8();
    Ok(single(to_color_image(rgba.width(), rgba.height(), rgba.as_raw()), orig))
}

const ANIM_BUDGET: usize = 1536 * 1024 * 1024; // байт на все кадры

fn collect_frames(frames: image::Frames<'_>, max_side: u32) -> Option<Content> {
    let mut out = Vec::new();
    let mut bytes = 0usize;
    let mut orig = [0, 0];
    for f in frames {
        let f = f.ok()?;
        let (n, d) = f.delay().numer_denom_ms();
        let mut delay = if d == 0 { 0.1 } else { n as f32 / d as f32 / 1000.0 };
        if delay < 0.011 {
            delay = 0.1; // как в браузерах
        }
        let buf = f.into_buffer();
        orig = [buf.width(), buf.height()];
        let img = if buf.width() > max_side || buf.height() > max_side {
            limit(DynamicImage::ImageRgba8(buf), max_side).into_rgba8()
        } else {
            buf
        };
        bytes += img.as_raw().len();
        out.push(Frame {
            image: Arc::new(to_color_image(img.width(), img.height(), img.as_raw())),
            delay,
        });
        if bytes > ANIM_BUDGET {
            break;
        }
    }
    if out.len() <= 1 {
        return None; // статичное — декодируем обычным путём (с учётом ориентации)
    }
    Some(Content::Image { frames: out, orig, note: None })
}

fn open_buf(path: &Path) -> Option<std::io::BufReader<std::fs::File>> {
    std::fs::File::open(path).ok().map(std::io::BufReader::new)
}

fn decode_gif(path: &Path, max_side: u32) -> Option<Content> {
    let dec = image::codecs::gif::GifDecoder::new(open_buf(path)?).ok()?;
    collect_frames(dec.into_frames(), max_side)
}

fn decode_apng(path: &Path, max_side: u32) -> Option<Content> {
    let dec = image::codecs::png::PngDecoder::new(open_buf(path)?).ok()?;
    if !dec.is_apng().ok()? {
        return None;
    }
    collect_frames(dec.apng().ok()?.into_frames(), max_side)
}

fn decode_webp_anim(path: &Path, max_side: u32) -> Option<Content> {
    let dec = image::codecs::webp::WebPDecoder::new(open_buf(path)?).ok()?;
    if !dec.has_animation() {
        return None;
    }
    collect_frames(dec.into_frames(), max_side)
}

fn decode_svg(path: &Path, max_side: u32) -> Result<Content, String> {
    use resvg::{tiny_skia, usvg};
    let data = std::fs::read(path).map_err(|e| e.to_string())?;
    let mut opt = usvg::Options::default();
    opt.resources_dir = path.parent().map(|p| p.to_path_buf());
    opt.fontdb_mut().load_system_fonts();
    let tree = usvg::Tree::from_data(&data, &opt).map_err(|e| e.to_string())?;
    let size = tree.size();
    // Растеризуем с запасом для чёткого масштабирования.
    let target = (size.width().max(size.height()) * 2.0).clamp(1024.0, max_side.min(4096) as f32);
    let scale = target / size.width().max(size.height());
    let w = (size.width() * scale).ceil().max(1.0) as u32;
    let h = (size.height() * scale).ceil().max(1.0) as u32;
    let mut pixmap = tiny_skia::Pixmap::new(w, h).ok_or(tr!("слишком большой SVG", "SVG is too large", "завеликий SVG"))?;
    resvg::render(&tree, tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
    let px: Vec<Color32> = pixmap
        .pixels()
        .iter()
        .map(|p| Color32::from_rgba_premultiplied(p.red(), p.green(), p.blue(), p.alpha()))
        .collect();
    let img = ColorImage::new([w as usize, h as usize], px);
    Ok(single(img, [size.width().round() as u32, size.height().round() as u32]))
}

const TEXT_LIMIT: usize = 4 * 1024 * 1024;

fn decode_text(path: &Path) -> Content {
    let mut f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) => return Content::Error(e.to_string()),
    };
    let mut buf = Vec::new();
    let _ = f.by_ref().take(TEXT_LIMIT as u64 + 1).read_to_end(&mut buf);
    let truncated = buf.len() > TEXT_LIMIT;
    buf.truncate(TEXT_LIMIT);
    Content::Text { text: bytes_to_text(&buf), truncated }
}

fn bytes_to_text(buf: &[u8]) -> String {
    if buf.starts_with(&[0xFF, 0xFE]) || buf.starts_with(&[0xFE, 0xFF]) {
        let le = buf[0] == 0xFF;
        let u: Vec<u16> = buf[2..]
            .chunks_exact(2)
            .map(|c| if le { u16::from_le_bytes([c[0], c[1]]) } else { u16::from_be_bytes([c[0], c[1]]) })
            .collect();
        return String::from_utf16_lossy(&u);
    }
    let b = buf.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(buf);
    match std::str::from_utf8(b) {
        Ok(s) => s.to_string(),
        Err(_) => {
            // Windows-1251 — самая частая не-UTF8 кодировка для русских текстов.
            b.iter().map(|&c| cp1251(c)).collect()
        }
    }
}

fn cp1251(c: u8) -> char {
    match c {
        0..=0x7F => c as char,
        0xC0..=0xFF => char::from_u32(0x410 + (c as u32 - 0xC0)).unwrap_or('?'),
        0xA8 => 'Ё',
        0xB8 => 'ё',
        0xB9 => '№',
        0xAB => '«',
        0xBB => '»',
        0x96 => '–',
        0x97 => '—',
        0xA0 => '\u{a0}',
        _ => '?',
    }
}

fn looks_textual(path: &Path) -> bool {
    let mut buf = [0u8; 8192];
    let Ok(mut f) = std::fs::File::open(path) else { return false };
    let Ok(n) = f.read(&mut buf) else { return false };
    let b = &buf[..n];
    if b.starts_with(&[0xFF, 0xFE]) || b.starts_with(&[0xFE, 0xFF]) {
        return true;
    }
    n > 0 && !b.contains(&0) && b.iter().filter(|&&c| c < 9 || (c > 13 && c < 32)).count() < n / 100 + 1
}

/// Неизвестное расширение: пробуем всё подряд.
fn decode_unknown(path: &Path, max_side: u32) -> Content {
    if let Ok(c) = decode_with_image_crate(path, max_side) {
        return c;
    }
    if let Ok(info) = ffmpeg::probe(path) {
        let is_still = info.has_video && info.duration <= 0.05 && !info.has_audio;
        if !is_still {
            return Content::Media { info, cover: None };
        }
    }
    if let Ok((r, _)) = winutil::wic_decode(path) {
        let (img, orig) = rgba_limit(r, max_side);
        return single(img, orig);
    }
    if ffmpeg::ffmpeg().is_some() {
        if let Ok(r) = ffmpeg::decode_still(path, "0:v:0", max_side) {
            let (img, orig) = rgba_limit(r, max_side);
            return single(img, orig);
        }
    }
    if looks_textual(path) {
        return decode_text(path);
    }
    match winutil::shell_thumbnail(path, 1024) {
        Some(r) => {
            let (img, _) = rgba_limit(r, max_side);
            Content::Preview { image: Some(Arc::new(img)) }
        }
        None => Content::Preview { image: None },
    }
}

fn read_exif(path: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let Ok(file) = std::fs::File::open(path) else { return out };
    let mut r = std::io::BufReader::new(file);
    let Ok(ex) = exif::Reader::new().read_from_container(&mut r) else { return out };
    use exif::{In, Tag};
    let get = |t: Tag| ex.get_field(t, In::PRIMARY).map(|f| f.display_value().with_unit(&ex).to_string());
    let clean = |s: String| s.trim_matches('"').trim().to_string();
    let make = get(Tag::Make).map(clean).unwrap_or_default();
    let model = get(Tag::Model).map(clean).unwrap_or_default();
    if !model.is_empty() {
        let cam = if !make.is_empty() && !model.to_lowercase().starts_with(&make.to_lowercase()) {
            format!("{make} {model}")
        } else {
            model
        };
        out.push((tr!("Камера", "Camera", "Камера").into(), cam));
    }
    if let Some(v) = get(Tag::LensModel) {
        out.push((tr!("Объектив", "Lens", "Об'єктив").into(), clean(v)));
    }
    let mut exp = Vec::new();
    if let Some(v) = get(Tag::FNumber) {
        exp.push(v.replace(' ', ""));
    }
    if let Some(v) = get(Tag::ExposureTime) {
        exp.push(v);
    }
    if let Some(v) = get(Tag::PhotographicSensitivity) {
        exp.push(format!("ISO {v}"));
    }
    if !exp.is_empty() {
        out.push((tr!("Экспозиция", "Exposure", "Експозиція").into(), exp.join(" · ")));
    }
    if let Some(v) = get(Tag::FocalLength) {
        out.push((tr!("Фокусное расстояние", "Focal length", "Фокусна відстань").into(), v));
    }
    if let Some(v) = get(Tag::DateTimeOriginal) {
        out.push((tr!("Дата съёмки", "Date taken", "Дата зйомки").into(), v));
    }
    out
}

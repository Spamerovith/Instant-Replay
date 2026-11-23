//! Классификация файлов по расширению.

use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Image,
    Video,
    Audio,
    Pdf,
    Text,
    Other,
}

/// Растровые/векторные изображения (включая анимированные).
pub const IMAGE: &[&str] = &[
    "jpg", "jpeg", "jpe", "jfif", "pjpeg", "pjp", "png", "apng", "gif", "webp", "bmp", "dib",
    "rle", "ico", "cur", "tif", "tiff", "tga", "icb", "vda", "vst", "dds", "hdr", "pic", "exr",
    "pbm", "pgm", "ppm", "pnm", "pam", "pfm", "qoi", "ff", "farbfeld", "svg", "svgz", "avif",
    "avifs", "heic", "heif", "hif", "jxl", "jp2", "j2k", "jpf", "jpx", "jpm", "j2c", "jxr",
    "wdp", "hdp", "psd", "psb", "xcf", "pcx", "sgi", "rgb", "rgba", "bw", "ras", "sun", "xbm",
    "xpm", "xwd", "wbmp", "emf", "wmf", "jng", "mng", "fits", "fit", "fts", "dpx", "cin",
    "pix", "jls", "ktx", "ktx2", "vtf", "pcd", "msp", "ora", "kra",
    // RAW камер (через WIC/кодеки Windows и ffmpeg)
    "dng", "cr2", "cr3", "crw", "nef", "nrw", "arw", "srf", "sr2", "orf", "rw2", "raf", "pef",
    "ptx", "srw", "x3f", "erf", "mrw", "mef", "mos", "kdc", "dcr", "3fr", "fff", "iiq", "rwl",
    "raw",
];

pub const VIDEO: &[&str] = &[
    "mp4", "m4v", "mkv", "mk3d", "webm", "mov", "qt", "avi", "wmv", "asf", "flv", "f4v", "mpg",
    "mpeg", "mpe", "m1v", "m2v", "mpv", "ts", "mts", "m2ts", "m2t", "tp", "trp", "vob", "ogv",
    "3gp", "3g2", "3gpp", "divx", "xvid", "rm", "rmvb", "dv", "mxf", "nut", "y4m", "ivf",
    "h264", "264", "h265", "265", "hevc", "av1", "obu", "vp8", "vp9", "amv", "bik", "bk2",
    "smk", "roq", "nsv", "dat", "evo", "wtv", "dvr-ms", "gifv", "swf", "mjpeg", "mjpg",
];

pub const AUDIO: &[&str] = &[
    "mp3", "wav", "wave", "flac", "ogg", "oga", "opus", "m4a", "m4b", "m4r", "aac", "wma",
    "aiff", "aif", "aifc", "alac", "ape", "wv", "mka", "ac3", "eac3", "dts", "dtshd", "thd",
    "mlp", "amr", "awb", "au", "snd", "caf", "mid", "midi", "mod", "xm", "it", "s3m", "mtm",
    "umx", "spx", "tta", "tak", "mpc", "ra", "voc", "w64", "rf64", "gsm", "8svx", "aa3", "oma",
    "dsf", "dff", "qoa", "weba", "vgm", "vgz", "nsf", "spc", "gbs", "ay", "kss", "sap",
];

pub const TEXT: &[&str] = &[
    "txt", "text", "log", "md", "markdown", "rst", "csv", "tsv", "json", "jsonc", "json5",
    "xml", "yaml", "yml", "toml", "ini", "cfg", "conf", "config", "properties", "env", "rs",
    "py", "pyw", "js", "mjs", "cjs", "ts", "tsx", "jsx", "c", "h", "cpp", "hpp", "cc", "hh",
    "cs", "java", "kt", "kts", "go", "rb", "php", "pl", "lua", "sh", "bash", "zsh", "ps1",
    "psm1", "bat", "cmd", "sql", "html", "htm", "css", "scss", "less", "vue", "svelte",
    "swift", "dart", "r", "m", "gradle", "cmake", "make", "mk", "dockerfile", "gitignore",
    "editorconfig", "srt", "vtt", "ass", "ssa", "sub", "nfo", "diz", "reg", "inf", "gd",
    "shader", "hlsl", "glsl", "wgsl", "asm", "s", "tex", "bib", "lst", "diff", "patch",
];

pub fn ext_of(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default()
}

pub fn kind_of(path: &Path) -> Kind {
    let e = ext_of(path);
    let e = e.as_str();
    // "ts" — чаще видео-поток (MPEG-TS), но может быть TypeScript: решаем по содержимому.
    if e == "ts" {
        return if looks_like_mpegts(path) { Kind::Video } else { Kind::Text };
    }
    if IMAGE.contains(&e) {
        Kind::Image
    } else if VIDEO.contains(&e) {
        Kind::Video
    } else if AUDIO.contains(&e) {
        Kind::Audio
    } else if e == "pdf" {
        Kind::Pdf
    } else if TEXT.contains(&e) {
        Kind::Text
    } else {
        Kind::Other
    }
}

fn looks_like_mpegts(path: &Path) -> bool {
    use std::io::Read;
    let mut buf = [0u8; 377];
    if let Ok(mut f) = std::fs::File::open(path) {
        if let Ok(n) = f.read(&mut buf) {
            return n >= 377 && buf[0] == 0x47 && buf[188] == 0x47 && buf[376] == 0x47;
        }
    }
    false
}

/// Показывать ли файл при листании папки.
/// Если открыт файл другого типа (текст, неизвестный) — листаются файлы того же расширения.
pub fn is_browsable(path: &Path, opened: &Path) -> bool {
    match kind_of(opened) {
        Kind::Text | Kind::Other => ext_of(path) == ext_of(opened),
        _ => !matches!(kind_of(path), Kind::Other | Kind::Text),
    }
}

/// Все расширения, которые регистрируются в Проводнике.
pub fn registered_exts() -> Vec<(&'static str, Kind)> {
    let mut v = Vec::new();
    v.extend(IMAGE.iter().map(|e| (*e, Kind::Image)));
    v.extend(VIDEO.iter().filter(|e| **e != "dat").map(|e| (*e, Kind::Video)));
    v.extend(AUDIO.iter().map(|e| (*e, Kind::Audio)));
    v.push(("pdf", Kind::Pdf));
    v
}

pub fn kind_label(k: Kind, ext: &str) -> String {
    let up = ext.to_ascii_uppercase();
    match k {
        Kind::Image => trf!(["Изображение {up}", "{up} image", "Зображення {up}"]),
        Kind::Video => trf!(["Видео {up}", "{up} video", "Відео {up}"]),
        Kind::Audio => trf!(["Аудио {up}", "{up} audio", "Аудіо {up}"]),
        Kind::Pdf => tr!("Документ PDF", "PDF document", "Документ PDF").into(),
        Kind::Text => trf!(["Текст {up}", "{up} text", "Текст {up}"]),
        Kind::Other if up.is_empty() => tr!("Файл", "File", "Файл").into(),
        Kind::Other => trf!(["Файл {up}", "{up} file", "Файл {up}"]),
    }
}

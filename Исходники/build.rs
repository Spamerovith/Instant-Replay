//! Embeds the application icon and version information into the executable.
//! The icon is drawn procedurally (same ring + dot as `src/icon.rs`), so no binary assets
//! live in the repository.

use std::io::Write;
use std::path::PathBuf;

fn rgba(size: u32) -> Vec<u8> {
    let s = size as f32;
    let c = s / 2.0;
    let (r_out, r_in, r_dot) = (s * 0.46, s * 0.36, s * 0.2);
    let col = [0xf5u8, 0xf5, 0xf7];
    let mut px = vec![0u8; (size * size * 4) as usize];
    for y in 0..size {
        for x in 0..size {
            let mut cov = 0.0f32;
            for sy in 0..4 {
                for sx in 0..4 {
                    let fx = x as f32 + (sx as f32 + 0.5) / 4.0 - c;
                    let fy = y as f32 + (sy as f32 + 0.5) / 4.0 - c;
                    let d = (fx * fx + fy * fy).sqrt();
                    if (d <= r_out && d >= r_in) || d <= r_dot {
                        cov += 1.0 / 16.0;
                    }
                }
            }
            let i = ((y * size + x) * 4) as usize;
            px[i..i + 3].copy_from_slice(&col);
            px[i + 3] = (cov * 255.0).round() as u8;
        }
    }
    px
}

fn ico() -> Vec<u8> {
    let sizes = [16u32, 20, 24, 32, 40, 48, 64, 128, 256];
    let mut images: Vec<Vec<u8>> = Vec::new();
    for &s in &sizes {
        let px = rgba(s);
        let mut b = Vec::new();
        b.extend_from_slice(&40u32.to_le_bytes());
        b.extend_from_slice(&(s as i32).to_le_bytes());
        b.extend_from_slice(&((s * 2) as i32).to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&32u16.to_le_bytes());
        b.extend_from_slice(&[0u8; 24]);
        for y in (0..s).rev() {
            for x in 0..s {
                let i = ((y * s + x) * 4) as usize;
                b.extend_from_slice(&[px[i + 2], px[i + 1], px[i], px[i + 3]]);
            }
        }
        let mask_row = ((s + 31) / 32) * 4;
        b.extend(std::iter::repeat(0u8).take((mask_row * s) as usize));
        images.push(b);
    }
    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&(sizes.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * sizes.len() as u32;
    for (i, &s) in sizes.iter().enumerate() {
        out.push(if s >= 256 { 0 } else { s as u8 });
        out.push(if s >= 256 { 0 } else { s as u8 });
        out.push(0);
        out.push(0);
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&32u16.to_le_bytes());
        out.extend_from_slice(&(images[i].len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += images[i].len() as u32;
    }
    for img in images {
        out.extend(img);
    }
    out
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=Cargo.toml");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    embed_tools(&out);
    let ico_path = out.join("app.ico");
    std::fs::write(&ico_path, ico()).unwrap();

    let version = std::env::var("CARGO_PKG_VERSION").unwrap();
    let mut parts: Vec<u16> = version.split('.').map(|p| p.parse().unwrap_or(0)).collect();
    parts.resize(4, 0);
    let comma = parts.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(",");
    let rc = format!(
        r#"#pragma code_page(65001)
1 ICON "{ico}"
1 VERSIONINFO
FILEVERSION {comma}
PRODUCTVERSION {comma}
FILEOS 0x40004
FILETYPE 0x1
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "041904B0"
    BEGIN
      VALUE "FileDescription", "Instant Replay"
      VALUE "ProductName", "Instant Replay"
      VALUE "FileVersion", "{version}"
      VALUE "ProductVersion", "{version}"
      VALUE "OriginalFilename", "Instant Replay.exe"
      VALUE "InternalName", "instant-replay"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x419, 1200
  END
END
"#,
        ico = ico_path.display().to_string().replace('\\', "\\\\"),
    );
    let rc_path = out.join("app.rc");
    std::fs::File::create(&rc_path).unwrap().write_all(rc.as_bytes()).unwrap();
    embed_resource::compile(&rc_path, embed_resource::NONE);
}


/// Lists the packed tools from `embed/` (JustView and FFmpeg, made by `prepare-embed.ps1`) as a Rust
/// table. A name like `ffmpeg__avcodec-63.dll.gz` is the file `ffmpeg/avcodec-63.dll`. An empty
/// folder (plain `cargo test`) gives an empty table and the app falls back to installed tools.
fn embed_tools(out: &std::path::Path) {
    println!("cargo:rerun-if-changed=embed");
    let mut names: Vec<_> = std::fs::read_dir("embed")
        .map(|d| d.flatten().map(|e| e.file_name().to_string_lossy().to_string()).filter(|n| n.ends_with(".gz")).collect())
        .unwrap_or_default();
    names.sort();
    let mut src = String::from("pub static EMBEDDED: &[(&str, &[u8])] = &[\n");
    for n in names {
        let rel = n.trim_end_matches(".gz").replace("__", "/");
        let abs = std::fs::canonicalize(format!("embed/{n}")).unwrap();
        let abs = abs.to_string_lossy().trim_start_matches(r"\\?\").replace('\\', "/");
        src.push_str(&format!("    ({rel:?}, include_bytes!({abs:?})),\n"));
    }
    src.push_str("];\n");
    std::fs::write(out.join("embedded_tools.rs"), src).unwrap();
}

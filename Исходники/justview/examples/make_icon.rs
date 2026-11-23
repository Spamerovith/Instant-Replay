//! Генерирует assets/icon.ico и assets/icon-256.png из SVG.
//! Запуск: cargo run --release --example make_icon

use image::ExtendedColorType;
use image::codecs::ico::{IcoEncoder, IcoFrame};
use resvg::{tiny_skia, usvg};

fn render(svg: &str, size: u32) -> Vec<u8> {
    let tree = usvg::Tree::from_str(svg, &usvg::Options::default()).expect("svg");
    let mut pixmap = tiny_skia::Pixmap::new(size, size).unwrap();
    let s = size as f32 / tree.size().width();
    resvg::render(&tree, tiny_skia::Transform::from_scale(s, s), &mut pixmap.as_mut());
    let mut out = Vec::with_capacity((size * size * 4) as usize);
    for p in pixmap.pixels() {
        let c = p.demultiply();
        out.extend_from_slice(&[c.red(), c.green(), c.blue(), c.alpha()]);
    }
    out
}

fn main() {
    let big = include_str!("../assets/icon.svg");
    let small = include_str!("../assets/icon-small.svg");
    let sizes = [16u32, 20, 24, 32, 40, 48, 64, 96, 128, 256];
    let mut pngs = Vec::new();
    for &s in &sizes {
        let rgba = render(if s <= 32 { small } else { big }, s);
        pngs.push((s, rgba));
    }
    let frames: Vec<IcoFrame> = pngs
        .iter()
        .map(|(s, rgba)| IcoFrame::as_png(rgba, *s, *s, ExtendedColorType::Rgba8).unwrap())
        .collect();
    let f = std::fs::File::create("assets/icon.ico").unwrap();
    IcoEncoder::new(f).encode_images(&frames).unwrap();

    let (_, rgba256) = pngs.last().unwrap();
    image::save_buffer("assets/icon-256.png", rgba256, 256, 256, ExtendedColorType::Rgba8).unwrap();
    let rgba1024 = render(big, 1024);
    image::save_buffer("assets/icon-1024.png", &rgba1024, 1024, 1024, ExtendedColorType::Rgba8).unwrap();
    println!("icon.ico, icon-256.png, icon-1024.png written");
}

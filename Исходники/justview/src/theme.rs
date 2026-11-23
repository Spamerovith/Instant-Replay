//! Токены интерфейса. Источник значений — DESIGN.md.
#![allow(dead_code)]

use egui::{Color32, FontData, FontDefinitions, FontFamily, FontId};
use std::sync::Arc;

// ---- Color ----
pub const CANVAS: Color32 = Color32::from_rgb(0, 0, 0);
pub const SURFACE: Color32 = Color32::from_rgb(0x16, 0x16, 0x17);
pub const SURFACE_RAISED: Color32 = Color32::from_rgb(0x1d, 0x1d, 0x1f);
pub const SURFACE_PRESSED: Color32 = Color32::from_rgb(0x2a, 0x2a, 0x2d);
pub const GLASS: Color32 = Color32::from_rgba_premultiplied(19, 19, 20, 219); // rgb(22 22 23 / 0.86)
pub const TEXT: Color32 = Color32::from_rgb(0xf5, 0xf5, 0xf7);
pub const TEXT_SECONDARY: Color32 = Color32::from_rgb(0xa1, 0xa1, 0xa6);
pub const HAIRLINE: Color32 = Color32::from_rgba_premultiplied(26, 26, 26, 26); // white / 0.1
pub const FIELD_BORDER: Color32 = Color32::from_rgb(0x86, 0x86, 0x8b);
pub const ACTION: Color32 = TEXT;
pub const ON_ACTION: Color32 = CANVAS;
pub const ACCENT: Color32 = Color32::from_rgb(0x29, 0x97, 0xff);
pub const DANGER: Color32 = Color32::from_rgb(0xff, 0x45, 0x3a);
pub const SCRIM: Color32 = Color32::from_rgba_premultiplied(0, 0, 0, 150);

// ---- Spacing ----
pub const SP_4: f32 = 4.0;
pub const SP_8: f32 = 8.0;
pub const SP_12: f32 = 12.0;
pub const SP_16: f32 = 16.0;
pub const SP_20: f32 = 20.0;
pub const SP_24: f32 = 24.0;
pub const SP_32: f32 = 32.0;
pub const SP_40: f32 = 40.0;
pub const SP_48: f32 = 48.0;
pub const SP_64: f32 = 64.0;

// ---- Radii ----
pub const R_CARD: u8 = 18;
pub const R_FIELD: u8 = 12;

// ---- Components ----
pub const ICON_BTN: f32 = 36.0;
pub const ICON_SIZE: f32 = 18.0;
pub const ICON_STROKE: f32 = 1.5;
pub const CAPSULE_H: f32 = 52.0;
pub const EDGE_BTN: f32 = 44.0;
pub const INFO_W: f32 = 340.0;
pub const ROW_H: f32 = 44.0;
pub const PILL_H: f32 = 44.0;

// ---- Motion (seconds) ----
pub const M_PRESS_MIN: f64 = 0.09;
pub const M_MICRO: f32 = 0.20;
pub const M_SMALL: f32 = 0.30;
pub const M_MEDIUM: f32 = 0.40;
pub const M_FADE: f32 = 0.16;
pub const CHROME_IDLE: f64 = 2.0;
pub const M_ZOOM_RATE: f32 = 18.0; // 1/s for exponential smoothing (~0.12 s)

// ---- Type ----
pub const T_TITLE: f32 = 22.0;
pub const T_HEADLINE: f32 = 17.0;
pub const T_BODY: f32 = 15.0;
pub const T_SUBHEAD: f32 = 14.0;
pub const T_FOOTNOTE: f32 = 13.0;
pub const T_CAPTION: f32 = 12.0;

pub fn medium(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("medium".into()))
}
pub fn light(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("light".into()))
}
pub fn regular(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}
pub fn mono(size: f32) -> FontId {
    FontId::new(size, FontFamily::Monospace)
}

pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let add = |fonts: &mut FontDefinitions, name: &str, data: &'static [u8]| {
        fonts
            .font_data
            .insert(name.to_owned(), Arc::new(FontData::from_static(data)));
    };
    add(&mut fonts, "geo340", include_bytes!("../assets/fonts/Geologica-340.ttf"));
    add(&mut fonts, "geo400", include_bytes!("../assets/fonts/Geologica-400.ttf"));
    add(&mut fonts, "geo500", include_bytes!("../assets/fonts/Geologica-500.ttf"));

    let fallback: Vec<String> = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();

    let mut fam = |name: FontFamily, first: &str| {
        let mut v = vec![first.to_owned()];
        v.extend(fallback.iter().cloned());
        fonts.families.insert(name, v);
    };
    fam(FontFamily::Proportional, "geo400");
    fam(FontFamily::Name("light".into()), "geo340");
    fam(FontFamily::Name("medium".into()), "geo500");

    // Monospace: системный Consolas, если есть.
    let mut mono = Vec::new();
    for p in [r"C:\Windows\Fonts\CascadiaMono.ttf", r"C:\Windows\Fonts\consola.ttf"] {
        if let Ok(bytes) = std::fs::read(p) {
            fonts
                .font_data
                .insert("mono".into(), Arc::new(FontData::from_owned(bytes)));
            mono.push("mono".to_owned());
            break;
        }
    }
    mono.push("geo400".to_owned());
    mono.extend(fallback.iter().cloned());
    fonts.families.insert(FontFamily::Monospace, mono);

    ctx.set_fonts(fonts);
}

pub fn install_style(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = CANVAS;
    visuals.window_fill = SURFACE;
    visuals.extreme_bg_color = SURFACE_RAISED;
    visuals.override_text_color = Some(TEXT);
    visuals.selection.bg_fill = ACCENT.gamma_multiply(0.45);
    visuals.selection.stroke = egui::Stroke::new(1.0, TEXT);
    visuals.hyperlink_color = ACCENT;
    visuals.window_stroke = egui::Stroke::NONE;
    visuals.popup_shadow = egui::epaint::Shadow::NONE;
    visuals.window_shadow = egui::epaint::Shadow::NONE;
    ctx.set_visuals(visuals);
    ctx.global_style_mut(|s| {
        s.spacing.scroll.bar_width = 6.0;
        s.spacing.scroll.floating = true;
        s.text_styles.insert(egui::TextStyle::Body, light(T_BODY));
        s.text_styles.insert(egui::TextStyle::Button, regular(T_SUBHEAD));
        s.text_styles.insert(egui::TextStyle::Monospace, mono(T_SUBHEAD));
        s.text_styles.insert(egui::TextStyle::Small, regular(T_CAPTION));
        s.text_styles.insert(egui::TextStyle::Heading, medium(T_HEADLINE));
    });
}

/// Отключены ли в Windows «Эффекты анимации».
pub fn reduced_motion() -> bool {
    crate::winutil::animations_disabled()
}

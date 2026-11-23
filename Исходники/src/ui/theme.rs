//! Design tokens — mirror of DESIGN.md. Change values here and there together.
#![allow(dead_code)]

use egui::{Color32, FontFamily, FontId};

pub mod color {
    use egui::Color32;
    pub const CANVAS: Color32 = Color32::from_rgb(0x00, 0x00, 0x00);
    pub const SURFACE: Color32 = Color32::from_rgb(0x16, 0x16, 0x17);
    pub const SURFACE_RAISED: Color32 = Color32::from_rgb(0x1d, 0x1d, 0x1f);
    pub const SURFACE_HOVER: Color32 = Color32::from_rgb(0x23, 0x23, 0x25);
    pub const SURFACE_PRESSED: Color32 = Color32::from_rgb(0x2a, 0x2a, 0x2c);
    pub const TEXT: Color32 = Color32::from_rgb(0xf5, 0xf5, 0xf7);
    pub const TEXT_SECONDARY: Color32 = Color32::from_rgb(0xa1, 0xa1, 0xa6);
    pub const TEXT_DISABLED: Color32 = Color32::from_rgb(0x6e, 0x6e, 0x73);
    pub const HAIRLINE: Color32 = Color32::from_rgba_premultiplied(26, 26, 26, 26);
    pub const FIELD_BORDER: Color32 = Color32::from_rgb(0x86, 0x86, 0x8b);
    pub const FOCUS: Color32 = Color32::from_rgb(0x29, 0x97, 0xff);
    pub const RECORD: Color32 = Color32::from_rgb(0xff, 0x45, 0x3a);
    pub const SUCCESS: Color32 = Color32::from_rgb(0x30, 0xd1, 0x58);
    pub const WARNING: Color32 = Color32::from_rgb(0xff, 0x9f, 0x0a);
    pub const SLIDER_TRACK: Color32 = Color32::from_rgb(0x3a, 0x3a, 0x3c);
    pub const TRACK_HOVER: Color32 = Color32::from_rgb(0x48, 0x48, 0x4a);
    pub const SEGMENT_SELECTED: Color32 = Color32::from_rgb(0x6e, 0x6e, 0x73);
    pub const ACTION: Color32 = Color32::from_rgb(0xf5, 0xf5, 0xf7);
    pub const ACTION_PRESSED: Color32 = Color32::from_rgb(0xd2, 0xd2, 0xd7);
    pub const ACTION_LABEL: Color32 = Color32::from_rgb(0x00, 0x00, 0x00);
    /// Main window: graphite like the overlay panel, the menu column a step darker.
    pub const WINDOW: Color32 = Color32::from_rgb(0x16, 0x16, 0x17);
    pub const SIDEBAR: Color32 = Color32::from_rgb(0x11, 0x11, 0x12);
    /// Cards and grouped lists on the window, outlined by a faint light line.
    pub const CARD: Color32 = Color32::from_rgb(0x1e, 0x1e, 0x20);
    pub const CARD_BORDER: Color32 = Color32::from_rgba_premultiplied(15, 15, 15, 15);
    /// Fields inside cards: dropdowns, segmented controls, keycaps, menus.
    pub const FIELD: Color32 = Color32::from_rgb(0x2a, 0x2a, 0x2c);
    pub const FIELD_HOVER: Color32 = Color32::from_rgb(0x32, 0x32, 0x35);
    pub const FIELD_PRESSED: Color32 = Color32::from_rgb(0x3a, 0x3a, 0x3d);
}

pub mod space {
    pub const S4: f32 = 4.0;
    pub const S8: f32 = 8.0;
    pub const S12: f32 = 12.0;
    pub const S16: f32 = 16.0;
    pub const S20: f32 = 20.0;
    pub const S24: f32 = 24.0;
    pub const S32: f32 = 32.0;
    pub const S40: f32 = 40.0;
}

pub mod radius {
    pub const CARD: f32 = 20.0;
    pub const GROUP: f32 = 16.0;
    pub const INPUT: f32 = 10.0;
    pub const PILL: f32 = 999.0;
}

pub mod size {
    pub const BUTTON_LG: f32 = 40.0;
    pub const BUTTON_MD: f32 = 32.0;
    pub const BUTTON_SM: f32 = 26.0;
    pub const ROW_MIN: f32 = 48.0;
    pub const SWITCH_W: f32 = 40.0;
    pub const SWITCH_H: f32 = 24.0;
    pub const SWITCH_THUMB: f32 = 18.0;
    pub const SEGMENTED_H: f32 = 30.0;
    pub const SLIDER_TRACK: f32 = 4.0;
    pub const SLIDER_THUMB: f32 = 18.0;
    pub const NAV_ITEM: f32 = 36.0;
    pub const SIDEBAR_W: f32 = 220.0;
    pub const KEYCAP_H: f32 = 30.0;
    pub const KEYCAP_W: f32 = 168.0;
    pub const OVERLAY_W: f32 = 400.0;
    pub const OVERLAY_ROW: f32 = 48.0;
    pub const OVERLAY_TILE_H: f32 = 72.0;
    pub const PAGE_PAD: f32 = 32.0;
    pub const CONTENT_MAX_W: f32 = 760.0;
}

/// Motion durations in seconds (multiplied by the reduced-motion factor at runtime).
pub mod motion {
    pub const PRESS_DOWN: f32 = 0.08;
    pub const PRESS_RELEASE: f32 = 0.24;
    pub const PRESS_MIN_HOLD: f32 = 0.09;
    pub const PRESS_SCALE: f32 = 0.96;
    pub const HOVER: f32 = 0.16;
    pub const SWITCH: f32 = 0.20;
    pub const INDICATOR: f32 = 0.26;
    pub const VALUE: f32 = 0.30;
    pub const PAGE: f32 = 0.16;
}

pub const SEMIBOLD: &str = "semibold";
pub const ICONS: &str = "icons";

fn semibold() -> FontFamily {
    FontFamily::Name(SEMIBOLD.into())
}

pub fn display() -> FontId {
    FontId::new(40.0, semibold())
}
pub fn title() -> FontId {
    FontId::new(22.0, semibold())
}
pub fn headline() -> FontId {
    FontId::new(15.0, semibold())
}
pub fn body() -> FontId {
    FontId::new(14.0, FontFamily::Proportional)
}
pub fn body_strong() -> FontId {
    FontId::new(14.0, semibold())
}
pub fn subhead_strong() -> FontId {
    FontId::new(13.0, semibold())
}
pub fn subhead() -> FontId {
    FontId::new(13.0, FontFamily::Proportional)
}
pub fn caption() -> FontId {
    FontId::new(12.0, FontFamily::Proportional)
}
pub fn icon(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(ICONS.into()))
}

/// Segoe Fluent Icons / MDL2 code points.
pub mod glyph {
    pub const HOME: &str = "\u{E80F}";
    pub const VIDEO: &str = "\u{E714}";
    pub const VOLUME: &str = "\u{E767}";
    pub const KEYBOARD: &str = "\u{E765}";
    pub const SETTINGS: &str = "\u{E713}";
    pub const FOLDER: &str = "\u{E838}";
    pub const REFRESH: &str = "\u{E72C}";
    pub const WARNING: &str = "\u{E7BA}";
    pub const CHEVRON_DOWN: &str = "\u{E70D}";
    pub const CHEVRON_RIGHT: &str = "\u{E76C}";
    pub const CHEVRON_LEFT: &str = "\u{E76B}";
    pub const CAMERA: &str = "\u{E722}";
    pub const CHECK: &str = "\u{E73E}";
    pub const MONITOR: &str = "\u{E7F4}";
    pub const WINDOW: &str = "\u{E737}";
    pub const GAME: &str = "\u{E7FC}";
    pub const MIC: &str = "\u{E720}";
    pub const FILM: &str = "\u{E8B2}";
    pub const CLOSE: &str = "\u{E711}";
    pub const PLAY: &str = "\u{E768}";
    pub const STOP: &str = "\u{E71A}";
    pub const HISTORY: &str = "\u{E81C}";
    pub const BACK: &str = "\u{E72B}";
    pub const PAUSE: &str = "\u{E769}";
    pub const REPLAY: &str = "\u{E72C}";
    pub const STEP_BACK: &str = "\u{E892}";
    pub const STEP_FWD: &str = "\u{E893}";
    pub const MUTE: &str = "\u{E74F}";
    pub const FULLSCREEN: &str = "\u{E740}";
    pub const BACK_TO_WINDOW: &str = "\u{E73F}";
    pub const AUDIO: &str = "\u{E8D6}";
    pub const DOWNLOAD: &str = "\u{E896}";
    pub const DELETE: &str = "\u{E74D}";
}

fn read_font(names: &[&str]) -> Option<Vec<u8>> {
    let dir = std::env::var_os("WINDIR").map(std::path::PathBuf::from).unwrap_or_else(|| "C:\\Windows".into()).join("Fonts");
    names.iter().find_map(|n| std::fs::read(dir.join(n)).ok())
}

pub fn install(ctx: &egui::Context, reduced_motion: bool) {
    let mut fonts = egui::FontDefinitions::default();
    if let Some(b) = read_font(&["segoeui.ttf"]) {
        fonts.font_data.insert("segoe".into(), egui::FontData::from_owned(b));
        fonts.families.get_mut(&FontFamily::Proportional).unwrap().insert(0, "segoe".into());
    }
    let mut semi = Vec::new();
    if let Some(b) = read_font(&["seguisb.ttf", "segoeuib.ttf"]) {
        fonts.font_data.insert("segoe-semibold".into(), egui::FontData::from_owned(b));
        semi.push("segoe-semibold".to_string());
    }
    semi.extend(fonts.families[&FontFamily::Proportional].iter().cloned());
    fonts.families.insert(FontFamily::Name(SEMIBOLD.into()), semi);
    let mut icons = Vec::new();
    if let Some(b) = read_font(&["SegoeIcons.ttf", "segmdl2.ttf"]) {
        fonts.font_data.insert("segoe-icons".into(), egui::FontData::from_owned(b));
        icons.push("segoe-icons".to_string());
    }
    icons.extend(fonts.families[&FontFamily::Proportional].iter().cloned());
    fonts.families.insert(FontFamily::Name(ICONS.into()), icons);
    // Symbols (arrows, ·) fall back to Segoe UI Symbol when present.
    if let Some(b) = read_font(&["seguisym.ttf"]) {
        fonts.font_data.insert("segoe-symbol".into(), egui::FontData::from_owned(b));
        for fam in [FontFamily::Proportional, FontFamily::Name(SEMIBOLD.into())] {
            fonts.families.get_mut(&fam).unwrap().push("segoe-symbol".into());
        }
    }
    ctx.set_fonts(fonts);

    let mut style = (*ctx.style()).clone();
    style.animation_time = if reduced_motion { 0.0 } else { motion::HOVER };
    style.spacing.item_spacing = egui::vec2(space::S8, space::S8);
    style.spacing.button_padding = egui::vec2(space::S12, space::S4);
    style.spacing.interact_size.y = size::BUTTON_MD;
    style.spacing.scroll = egui::style::ScrollStyle::floating();
    style.text_styles.insert(egui::TextStyle::Body, body());
    style.text_styles.insert(egui::TextStyle::Button, body());
    style.text_styles.insert(egui::TextStyle::Small, caption());
    style.text_styles.insert(egui::TextStyle::Heading, title());
    let v = &mut style.visuals;
    *v = egui::Visuals::dark();
    v.panel_fill = color::WINDOW;
    v.window_fill = color::FIELD;
    v.extreme_bg_color = color::FIELD;
    v.faint_bg_color = color::SURFACE;
    v.override_text_color = Some(color::TEXT);
    v.hyperlink_color = color::FOCUS;
    v.selection.bg_fill = color::FOCUS.gamma_multiply(0.35);
    v.selection.stroke = egui::Stroke::new(1.0_f32, color::FOCUS);
    v.window_rounding = egui::Rounding::same(radius::INPUT + 2.0);
    v.menu_rounding = egui::Rounding::same(radius::INPUT + 2.0);
    v.window_stroke = egui::Stroke::new(1.0_f32, color::HAIRLINE);
    v.window_shadow = egui::epaint::Shadow { offset: egui::vec2(0.0, 8.0), blur: 24.0, spread: 0.0, color: Color32::from_black_alpha(140) };
    v.popup_shadow = v.window_shadow;
    for w in [&mut v.widgets.inactive, &mut v.widgets.noninteractive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] {
        w.rounding = egui::Rounding::same(radius::INPUT);
        w.fg_stroke = egui::Stroke::new(1.0_f32, color::TEXT);
        w.bg_stroke = egui::Stroke::NONE;
        w.expansion = 0.0;
    }
    v.widgets.noninteractive.bg_fill = color::SURFACE;
    v.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0_f32, color::TEXT_SECONDARY);
    v.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0_f32, color::HAIRLINE);
    v.widgets.inactive.bg_fill = color::FIELD;
    v.widgets.inactive.weak_bg_fill = color::FIELD;
    v.widgets.hovered.bg_fill = color::FIELD_HOVER;
    v.widgets.hovered.weak_bg_fill = color::FIELD_HOVER;
    v.widgets.active.bg_fill = color::FIELD_PRESSED;
    v.widgets.active.weak_bg_fill = color::FIELD_PRESSED;
    v.widgets.open.bg_fill = color::FIELD_HOVER;
    v.widgets.open.weak_bg_fill = color::FIELD_HOVER;
    // Text is never selectable here: selectable labels swallow clicks on clickable rows.
    style.interaction.selectable_labels = false;
    ctx.set_style(style);
}

/// Windows "Show animations in Windows" setting.
pub fn system_reduced_motion() -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{SystemParametersInfoW, SPI_GETCLIENTAREAANIMATION, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS};
    let mut on = windows::Win32::Foundation::BOOL(1);
    unsafe {
        let _ = SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, Some(&mut on as *mut _ as _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
    }
    !on.as_bool()
}

//! Встроенный плеер клипов (движок JustView): видео, перемотка, скорость 0,25–3×,
//! громкость и выбор звуковой дорожки (общий звук / система / микрофон).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use egui::{pos2, vec2, Align, Align2, Color32, CursorIcon, Id, Key, Layout, Rect, Response, RichText, Rounding, Sense, Shape, Stroke, TextureHandle, TextureOptions, Ui};

use super::theme::{self, color, glyph, motion, radius, size, space};
use super::widgets as w;
use crate::player::output::{AudioOut, Shared};
use crate::player::{ffmpeg, Player, SPEEDS};
use crate::util;

/// Настройки плеера, которые живут между открытиями клипов.
pub struct ViewerPrefs {
    pub volume: f32,
    pub muted: bool,
    pub speed: f32,
}

impl Default for ViewerPrefs {
    fn default() -> Self {
        ViewerPrefs { volume: 1.0, muted: false, speed: 1.0 }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Menu {
    None,
    Speed,
    Track,
}

const BAR_H: f32 = 96.0;
const TOP_H: f32 = 56.0;
const MENU_ROW: f32 = 34.0;

pub struct Viewer {
    pub path: PathBuf,
    title: String,
    player: Option<Player>,
    error: Option<String>,
    tex: Option<TextureHandle>,
    audio: Option<Arc<Shared>>,
    _out: Option<AudioOut>,
    menu: Menu,
    scrubbing: Option<bool>,
}

impl Viewer {
    pub fn open(ctx: &egui::Context, path: &Path, prefs: &ViewerPrefs) -> Self {
        let title = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        let mut v = Viewer {
            path: path.to_path_buf(),
            title,
            player: None,
            error: None,
            tex: None,
            audio: None,
            _out: None,
            menu: Menu::None,
            scrubbing: None,
        };
        if !path.is_file() {
            v.error = Some(tr!("Файл не найден - возможно, его переместили или удалили", "File not found - it may have been moved or deleted", "Файл не знайдено - можливо, його перемістили або видалили").into());
            return v;
        }
        match ffmpeg::probe(path) {
            Ok(info) => {
                let out = if info.has_audio { AudioOut::new() } else { None };
                v.audio = out.as_ref().map(|a| a.shared.clone());
                v._out = out;
                v.apply_volume(prefs);
                v.player = Some(Player::new(path, info, v.audio.clone(), ctx, [2560, 1600], prefs.speed, false));
            }
            Err(e) => v.error = Some(e),
        }
        v
    }

    fn apply_volume(&self, prefs: &ViewerPrefs) {
        if let Some(a) = &self.audio {
            a.set_volume(if prefs.muted { 0.0 } else { prefs.volume });
        }
    }

    /// Рисует плеер на всё окно. Возвращает `true`, когда пользователь закрыл его.
    pub fn ui(&mut self, ui: &mut Ui, prefs: &mut ViewerPrefs) -> bool {
        let ctx = ui.ctx().clone();
        let mut close = false;

        // Плеер и кадр
        if let Some(pl) = &mut self.player {
            if let Some(dt) = pl.update() {
                ctx.request_repaint_after(Duration::from_secs_f64(dt));
            }
            if pl.frame_changed {
                if let Some(f) = &pl.current {
                    let img = egui::ImageData::Color(f.image.clone());
                    match &mut self.tex {
                        Some(t) if t.size() == f.image.size => t.set(img, TextureOptions::LINEAR),
                        _ => self.tex = Some(ctx.load_texture("clip_frame", img, TextureOptions::LINEAR)),
                    }
                }
            }
            if pl.error.is_some() && self.error.is_none() {
                self.error = pl.error.clone();
            }
        }

        let fs = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
        let full = ui.max_rect();
        let top_h = if fs { 0.0 } else { TOP_H };
        let top = Rect::from_min_size(full.min, vec2(full.width(), top_h));
        let bar = Rect::from_min_max(pos2(full.left(), full.bottom() - BAR_H), full.max);
        let stage = Rect::from_min_max(pos2(full.left(), top.bottom()), pos2(full.right(), bar.top()));
        let painter = ui.painter().clone();
        painter.rect_filled(full, Rounding::ZERO, color::CANVAS);

        // Верхняя строка: назад, имя, показать в папке
        if !fs {
            let inner = top.shrink2(vec2(space::S16, 0.0));
            ui.allocate_new_ui(egui::UiBuilder::new().max_rect(inner), |ui| {
                ui.horizontal_centered(|ui| {
                    ui.spacing_mut().item_spacing.x = space::S8;
                    if w::icon_button(ui, glyph::BACK, tr!("Назад  Esc", "Back  Esc", "Назад  Esc")).clicked() {
                        close = true;
                    }
                    let right_w = 150.0;
                    let max_w = (ui.available_width() - right_w).max(60.0);
                    ui.allocate_ui_with_layout(vec2(max_w, size::BUTTON_MD), Layout::left_to_right(Align::Center), |ui| {
                        ui.add(egui::Label::new(RichText::new(&self.title).font(theme::headline()).color(color::TEXT)).truncate());
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if w::link(ui, tr!("Показать в папке", "Show in folder", "Показати в теці")).clicked() {
                            util::open_in_explorer(&self.path, true);
                        }
                    });
                });
            });
        }

        // Сцена: кадр по центру с сохранением пропорций
        let stage_resp = ui.interact(stage, Id::new("viewer_stage"), Sense::click());
        if let Some(err) = &self.error {
            let c = stage.center();
            painter.text(c - vec2(0.0, 28.0), Align2::CENTER_CENTER, glyph::WARNING, theme::icon(28.0), color::WARNING);
            let g = ui.fonts(|f| f.layout(err.clone(), theme::body(), color::TEXT_SECONDARY, (stage.width() - 64.0).clamp(200.0, 520.0)));
            painter.galley(pos2(c.x - g.size().x / 2.0, c.y), g, color::TEXT_SECONDARY);
        } else if let (Some(tex), Some(pl)) = (&self.tex, &self.player) {
            if pl.has_video() {
                let [tw, th] = tex.size();
                let s = (stage.width() / tw as f32).min(stage.height() / th as f32);
                let r = Rect::from_center_size(stage.center(), vec2(tw as f32 * s, th as f32 * s));
                painter.image(tex.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            }
        } else if let Some(pl) = &self.player {
            if !pl.has_video() {
                painter.text(stage.center(), Align2::CENTER_CENTER, glyph::VOLUME, theme::icon(40.0), color::TEXT_SECONDARY);
            }
        }
        if self.player.is_some() && self.menu == Menu::None {
            if stage_resp.double_clicked() {
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fs));
            } else if stage_resp.clicked() {
                if let Some(pl) = &mut self.player {
                    pl.toggle();
                }
            }
        }

        // Нижняя панель
        painter.line_segment([bar.left_top(), bar.right_top()], Stroke::new(1.0_f32, color::SURFACE_RAISED));
        let (anchor_speed, anchor_track) = self.control_bar(ui, bar, fs, prefs);
        self.menus(ui, bar, anchor_speed, anchor_track, prefs);

        // Клавиатура
        if self.keys(ui, fs, prefs) {
            close = true;
        }
        close
    }

    fn control_bar(&mut self, ui: &mut Ui, bar: Rect, fs: bool, prefs: &mut ViewerPrefs) -> (Option<Rect>, Option<Rect>) {
        let ctx = ui.ctx().clone();
        let painter = ui.painter().clone();
        let Some(pl) = &mut self.player else { return (None, None) };
        let dur = pl.duration();
        let pos = pl.position();

        // Ряд 1: время и шкала
        let pad = space::S24;
        let row1 = Rect::from_min_size(bar.min + vec2(pad, space::S12), vec2(bar.width() - pad * 2.0, 24.0));
        let tw = 56.0;
        painter.text(pos2(row1.left(), row1.center().y), Align2::LEFT_CENTER, fmt_time(pos), theme::caption(), color::TEXT);
        painter.text(pos2(row1.right(), row1.center().y), Align2::RIGHT_CENTER, fmt_time(dur), theme::caption(), color::TEXT_SECONDARY);
        let seek_r = Rect::from_min_max(pos2(row1.left() + tw, row1.top()), pos2(row1.right() - tw, row1.bottom()));
        let seek = ui.interact(seek_r, Id::new("viewer_seek"), Sense::click_and_drag());
        let hv = ctx.animate_bool_with_time(Id::new("viewer_seek_h"), seek.hovered() || seek.dragged(), w::dur(motion::HOVER));
        let th = size::SLIDER_TRACK + 2.0 * hv;
        let track = Rect::from_center_size(seek_r.center(), vec2(seek_r.width(), th));
        painter.rect_filled(track, Rounding::same(radius::PILL), color::SLIDER_TRACK);
        let frac = if dur > 0.0 { (pos / dur).clamp(0.0, 1.0) as f32 } else { 0.0 };
        let played = Rect::from_min_max(track.min, pos2(track.left() + track.width() * frac, track.max.y));
        painter.rect_filled(played, Rounding::same(radius::PILL), color::FOCUS);
        if hv > 0.0 {
            painter.circle_filled(pos2(played.max.x, track.center().y), 6.0 * hv, color::TEXT);
        }
        let t_at = |x: f32| ((x - seek_r.left()) / seek_r.width()).clamp(0.0, 1.0) as f64 * dur;
        if seek.hovered() {
            ctx.set_cursor_icon(CursorIcon::PointingHand);
            if let Some(p) = seek.hover_pos() {
                let g = ui.fonts(|f| f.layout_no_wrap(fmt_time(t_at(p.x)), theme::caption(), color::TEXT));
                let tr = Rect::from_center_size(pos2(p.x, seek_r.top() - space::S12), g.size() + vec2(16.0, 8.0));
                painter.rect_filled(tr, Rounding::same(radius::PILL), color::SURFACE_RAISED);
                painter.galley(tr.center() - g.size() / 2.0, g, color::TEXT);
            }
        }
        if dur > 0.0 {
            if seek.drag_started() {
                self.scrubbing = Some(!pl.paused);
                pl.set_paused(true);
            }
            if seek.dragged() {
                if let Some(p) = seek.interact_pointer_pos() {
                    pl.scrub(t_at(p.x));
                }
            }
            if seek.drag_stopped() {
                let resume = self.scrubbing.take().unwrap_or(false);
                if let Some(p) = seek.interact_pointer_pos() {
                    pl.end_scrub(t_at(p.x), resume);
                }
            } else if seek.clicked() {
                if let Some(p) = seek.interact_pointer_pos() {
                    pl.seek(t_at(p.x));
                }
            }
        }

        // Ряд 2: кнопки
        let row2 = Rect::from_min_max(pos2(bar.left() + pad - space::S8, row1.bottom() + space::S8), pos2(bar.right() - pad + space::S8, bar.bottom() - space::S12));
        let mut speed_anchor = None;
        let mut track_anchor = None;
        let tracks: Vec<String> = pl.info.audio_tracks.iter().map(|t| t.label()).collect();
        let cur_track = pl.audio_track;
        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(row2), |ui| {
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = space::S4;
                let (g, tip) = if pl.ended {
                    (glyph::REPLAY, tr!("Сначала  Пробел", "Replay  Space", "Спочатку  Пробіл"))
                } else if pl.paused {
                    (glyph::PLAY, tr!("Воспроизвести  Пробел", "Play  Space", "Відтворити  Пробіл"))
                } else {
                    (glyph::PAUSE, tr!("Пауза  Пробел", "Pause  Space", "Пауза  Пробіл"))
                };
                if w::icon_button(ui, g, tip).clicked() {
                    pl.toggle();
                }
                if pl.has_video() {
                    if w::icon_button(ui, glyph::STEP_BACK, tr!("Кадр назад  ,", "Previous frame  ,", "Кадр назад  ,")).clicked() {
                        pl.step_back();
                    }
                    if w::icon_button(ui, glyph::STEP_FWD, tr!("Кадр вперёд  .", "Next frame  .", "Кадр уперед  .")).clicked() {
                        pl.step_forward();
                    }
                }
                if pl.info.has_audio {
                    ui.add_space(space::S8);
                    let vg = if prefs.muted || prefs.volume <= 0.001 { glyph::MUTE } else { glyph::VOLUME };
                    if w::icon_button(ui, vg, if prefs.muted { tr!("Включить звук  M", "Unmute  M", "Увімкнути звук  M") } else { tr!("Выключить звук  M", "Mute  M", "Вимкнути звук  M") }).clicked() {
                        prefs.muted = !prefs.muted;
                        if let Some(a) = &self.audio {
                            a.set_volume(if prefs.muted { 0.0 } else { prefs.volume });
                        }
                    }
                    let mut v = if prefs.muted { 0.0 } else { prefs.volume };
                    let r = w::slider(ui, "viewer_volume", &mut v, 0.0, 1.0, 0.01, 104.0).on_hover_text(trf!(["Громкость {:.0} %  ↑↓", "Volume {:.0} %  ↑↓", "Гучність {:.0} %  ↑↓"], v * 100.0));
                    if r.changed() {
                        prefs.volume = v;
                        prefs.muted = v <= 0.001;
                        if let Some(a) = &self.audio {
                            a.set_volume(v);
                        }
                    }
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = space::S8;
                    let fg = if fs { glyph::BACK_TO_WINDOW } else { glyph::FULLSCREEN };
                    if w::icon_button(ui, fg, tr!("Во весь экран  F", "Full screen  F", "На весь екран  F")).clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fs));
                    }
                    let non_default = (pl.speed - 1.0).abs() > 1e-3;
                    let r = menu_pill(ui, "viewer_speed", None, &fmt_speed(pl.speed), non_default, self.menu == Menu::Speed).on_hover_text(tr!("Скорость  [ ]", "Speed  [ ]", "Швидкість  [ ]"));
                    if r.clicked() {
                        self.menu = if self.menu == Menu::Speed { Menu::None } else { Menu::Speed };
                    }
                    speed_anchor = Some(r.rect);
                    if tracks.len() > 1 {
                        let full_name = &tracks[cur_track.min(tracks.len() - 1)];
                        let short = full_name.split_once(" (").map(|(a, _)| a).unwrap_or(full_name);
                        let r = menu_pill(ui, "viewer_track", Some(glyph::AUDIO), short, false, self.menu == Menu::Track)
                            .on_hover_text(trf!(["Звуковая дорожка: {full_name}  A", "Audio track: {full_name}  A", "Звукова доріжка: {full_name}  A"]));
                        if r.clicked() {
                            self.menu = if self.menu == Menu::Track { Menu::None } else { Menu::Track };
                        }
                        track_anchor = Some(r.rect);
                    }
                });
            });
        });
        (speed_anchor, track_anchor)
    }

    fn menus(&mut self, ui: &mut Ui, bar: Rect, speed_anchor: Option<Rect>, track_anchor: Option<Rect>, prefs: &mut ViewerPrefs) {
        let ctx = ui.ctx().clone();
        let Some(pl) = &mut self.player else { return };
        let (anchor, items, selected): (Option<Rect>, Vec<String>, usize) = match self.menu {
            Menu::None => return,
            Menu::Speed => (
                speed_anchor,
                SPEEDS.iter().rev().map(|s| if *s == 1.0 { tr!("1× обычная", "1× normal", "1× звичайна").to_string() } else { fmt_speed(*s) }).collect(),
                SPEEDS.iter().rev().position(|s| (*s - pl.speed).abs() < 1e-3).unwrap_or(usize::MAX),
            ),
            Menu::Track => (track_anchor, pl.info.audio_tracks.iter().map(|t| t.label()).collect(), pl.audio_track),
        };
        let Some(anchor) = anchor else {
            self.menu = Menu::None;
            return;
        };
        let widest = items
            .iter()
            .map(|t| ui.fonts(|f| f.layout_no_wrap(t.clone(), theme::subhead(), color::TEXT).size().x))
            .fold(0.0f32, f32::max);
        let mw = (widest + space::S32 + space::S24).clamp(140.0, 360.0);
        let mh = MENU_ROW * items.len() as f32 + space::S8 * 2.0;
        let open_t = ctx.animate_bool_with_time(Id::new(("viewer_menu", self.menu as u8)), true, w::dur(motion::INDICATOR));
        let lift = (1.0 - open_t) * 8.0;
        let left = (anchor.center().x - mw / 2.0).min(bar.right() - space::S8 - mw).max(bar.left() + space::S8);
        let mr = Rect::from_min_size(pos2(left, bar.top() - space::S8 - mh + lift), vec2(mw, mh));
        let mut chosen = None;
        egui::Area::new(Id::new("viewer_menu_area")).order(egui::Order::Foreground).fixed_pos(mr.min).show(&ctx, |ui| {
            ui.set_opacity(open_t);
            let (rect, _) = ui.allocate_exact_size(mr.size(), Sense::click());
            let p = ui.painter();
            p.rect_filled(rect, Rounding::same(radius::GROUP), color::SURFACE_RAISED);
            for (i, name) in items.iter().enumerate() {
                let r = Rect::from_min_size(pos2(rect.left() + space::S4, rect.top() + space::S8 + i as f32 * MENU_ROW), vec2(rect.width() - space::S8, MENU_ROW));
                let id = Id::new(("viewer_menu_row", self.menu as u8, i));
                let rr = ui.interact(r, id, Sense::click());
                let h = ctx.animate_bool_with_time(id.with("h"), rr.hovered(), w::dur(motion::HOVER));
                if h > 0.0 {
                    p.rect_filled(r, Rounding::same(radius::INPUT), color::SURFACE_HOVER.gamma_multiply(h));
                }
                let sel = i == selected;
                if sel {
                    let c = pos2(r.left() + space::S16, r.center().y);
                    p.add(Shape::line(vec![c + vec2(-4.0, 0.0), c + vec2(-1.0, 3.5), c + vec2(5.0, -4.0)], Stroke::new(1.5_f32, color::FOCUS)));
                }
                let g = ui.fonts(|f| f.layout_no_wrap(name.clone(), theme::subhead(), color::TEXT));
                let clip = Rect::from_min_max(pos2(r.left() + space::S32, r.top()), pos2(r.right() - space::S12, r.bottom()));
                p.with_clip_rect(clip).galley(pos2(clip.left(), r.center().y - g.size().y / 2.0), g, if sel { color::FOCUS } else { color::TEXT });
                if rr.hovered() {
                    ctx.set_cursor_icon(CursorIcon::PointingHand);
                }
                if rr.clicked() {
                    chosen = Some(i);
                }
            }
        });
        if let Some(i) = chosen {
            match self.menu {
                Menu::Speed => {
                    let s = SPEEDS[SPEEDS.len() - 1 - i];
                    pl.set_speed(s);
                    prefs.speed = s;
                }
                Menu::Track => pl.set_audio_track(i),
                Menu::None => {}
            }
            self.menu = Menu::None;
        } else if ctx.input(|i| i.pointer.any_pressed()) {
            if let Some(p) = ctx.input(|i| i.pointer.interact_pos()) {
                if !mr.contains(p) && !anchor.contains(p) {
                    self.menu = Menu::None;
                }
            }
        }
    }

    /// Горячие клавиши плеера. Возвращает `true` для «назад».
    fn keys(&mut self, ui: &mut Ui, fs: bool, prefs: &mut ViewerPrefs) -> bool {
        let ctx = ui.ctx().clone();
        if ctx.memory(|m| m.focused().is_some()) {
            // Фокус на контроле (Tab) — стрелки и пробел принадлежат ему.
            if ctx.input(|i| i.key_pressed(Key::Escape)) {
                ctx.memory_mut(|m| m.request_focus(Id::NULL));
            }
            return false;
        }
        let pressed = |k: Key| ctx.input(|i| i.key_pressed(k));
        if pressed(Key::Escape) {
            if self.menu != Menu::None {
                self.menu = Menu::None;
            } else if fs {
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
            } else {
                return true;
            }
        }
        if pressed(Key::F) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fs));
        }
        let Some(pl) = &mut self.player else { return false };
        if pressed(Key::Space) || pressed(Key::K) {
            pl.toggle();
        }
        let pos = pl.position();
        if pressed(Key::ArrowLeft) {
            pl.seek((pos - 5.0).max(0.0));
        }
        if pressed(Key::ArrowRight) {
            pl.seek(pos + 5.0);
        }
        if pressed(Key::J) {
            pl.seek((pos - 10.0).max(0.0));
        }
        if pressed(Key::L) {
            pl.seek(pos + 10.0);
        }
        if pressed(Key::Comma) {
            pl.step_back();
        }
        if pressed(Key::Period) {
            pl.step_forward();
        }
        let step = |dir: i32, cur: f32| {
            let i = SPEEDS.iter().position(|s| (*s - cur).abs() < 1e-3).unwrap_or(3) as i32;
            SPEEDS[(i + dir).clamp(0, SPEEDS.len() as i32 - 1) as usize]
        };
        if pressed(Key::OpenBracket) {
            let s = step(-1, pl.speed);
            pl.set_speed(s);
            prefs.speed = s;
        }
        if pressed(Key::CloseBracket) {
            let s = step(1, pl.speed);
            pl.set_speed(s);
            prefs.speed = s;
        }
        if pressed(Key::Backspace) {
            pl.set_speed(1.0);
            prefs.speed = 1.0;
        }
        if pressed(Key::A) && pl.info.audio_tracks.len() > 1 {
            let t = (pl.audio_track + 1) % pl.info.audio_tracks.len();
            pl.set_audio_track(t);
        }
        let mut vol_changed = false;
        if pressed(Key::M) {
            prefs.muted = !prefs.muted;
            vol_changed = true;
        }
        if pressed(Key::ArrowUp) {
            prefs.volume = (prefs.volume + 0.05).min(1.0);
            prefs.muted = false;
            vol_changed = true;
        }
        if pressed(Key::ArrowDown) {
            prefs.volume = (prefs.volume - 0.05).max(0.0);
            vol_changed = true;
        }
        if vol_changed {
            self.apply_volume(prefs);
        }
        false
    }
}

/// Пилюля-кнопка, открывающая меню (скорость, дорожка).
pub fn menu_pill(ui: &mut Ui, id: &str, icon: Option<&str>, text: &str, accent: bool, open: bool) -> Response {
    let font = theme::subhead();
    let g = ui.fonts(|f| f.layout_no_wrap(text.to_owned(), font.clone(), color::TEXT));
    let lead = if icon.is_some() { 22.0 } else { 0.0 };
    let width = (g.size().x + lead + space::S16 * 2.0).clamp(64.0, 200.0);
    let (rect, resp) = ui.allocate_exact_size(vec2(width, size::BUTTON_MD), Sense::click());
    let resp = resp.on_hover_cursor(CursorIcon::PointingHand);
    let ctx = ui.ctx().clone();
    let h = ctx.animate_bool_with_time(Id::new((id, "h")), resp.hovered() || open, w::dur(motion::HOVER));
    let down = resp.is_pointer_button_down_on();
    let p = ctx.animate_bool_with_time(Id::new((id, "p")), down, w::dur(if down { motion::PRESS_DOWN } else { motion::PRESS_RELEASE }));
    let s = 1.0 - (1.0 - motion::PRESS_SCALE) * p;
    let r = Rect::from_center_size(rect.center(), rect.size() * s);
    let fill = w::lerp_color(color::SURFACE_RAISED, color::SURFACE_HOVER, h);
    let fill = w::lerp_color(fill, color::SURFACE_PRESSED, p);
    let painter = ui.painter();
    painter.rect_filled(r, Rounding::same(radius::PILL), fill);
    let mut x = r.left() + space::S16;
    if let Some(icon) = icon {
        painter.text(pos2(x, r.center().y), Align2::LEFT_CENTER, icon, theme::icon(13.0), color::TEXT_SECONDARY);
        x += lead;
    }
    let clip = Rect::from_min_max(pos2(x, r.top()), pos2(r.right() - space::S12, r.bottom()));
    let col = if accent { color::FOCUS } else { color::TEXT };
    painter.with_clip_rect(clip).galley(pos2(x, r.center().y - g.size().y / 2.0), g, col);
    if resp.has_focus() {
        painter.rect_stroke(rect.expand(3.0), Rounding::same(radius::PILL), Stroke::new(2.0_f32, color::FOCUS));
    }
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, text));
    resp
}

pub fn fmt_speed(s: f32) -> String {
    format!("{}×", format!("{s}").replace('.', ","))
}

fn fmt_time(t: f64) -> String {
    let t = t.max(0.0) as u64;
    let (h, m, s) = (t / 3600, t / 60 % 60, t % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

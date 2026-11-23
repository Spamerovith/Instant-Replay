//! Quick view of a clip inside the overlay: the clip plays right over the game, Ctrl + wheel
//! zooms into the picture and dragging moves the zoomed picture. Opening the clip in JustView is
//! left to the program window.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use egui::{pos2, vec2, Align, Align2, Color32, CursorIcon, Id, Key, Layout, Rect, RichText, Rounding, Sense, TextureHandle, TextureOptions, Ui, Vec2};

use super::theme::{self, color, glyph, motion, radius, size, space};
use super::viewer::ViewerPrefs;
use super::widgets as w;
use crate::player::output::{AudioOut, Shared};
use crate::player::{ffmpeg, Player};
use crate::util;

pub const MAX_ZOOM: f32 = 8.0;
/// Playback speeds of the quick view.
pub const QUICK_SPEEDS: [f32; 6] = [0.5, 0.75, 1.0, 1.25, 1.5, 2.0];
const MENU_ROW: f32 = 34.0;

/// Menus above the control row, the same as JustView's: speed and audio track.
#[derive(Clone, Copy, PartialEq)]
enum Menu {
    None,
    Speed,
    Track,
}
const CONTROLS_H: f32 = 40.0;

pub enum QuickAction {
    None,
    Close,
    Delete,
    /// Show the previous (-1) or next (+1) clip of the list.
    Step(i32),
}

/// Width of the side arrows' column next to the picture.
pub const ARROW_COL: f32 = 56.0;
const ARROW_D: f32 = 44.0;

pub struct QuickView {
    pub path: PathBuf,
    game: String,
    file: String,
    bytes: u64,
    player: Option<Player>,
    error: Option<String>,
    tex: Option<TextureHandle>,
    audio: Option<Arc<Shared>>,
    _out: Option<AudioOut>,
    scrubbing: Option<bool>,
    /// 1 = whole picture; the visible part is `1 / zoom` of it around `center` (in 0..1 picture units).
    zoom: f32,
    center: Vec2,
    dragged: bool,
    /// Can step to the previous / next clip.
    has_prev: bool,
    has_next: bool,
    /// Result of the last frame screenshot and when it arrived (shown for a moment).
    shot: Arc<parking_lot::Mutex<Option<Result<PathBuf, String>>>>,
    shot_note: Option<(Result<PathBuf, String>, std::time::Instant)>,
    menu: Menu,
    speed_anchor: Option<Rect>,
    track_anchor: Option<Rect>,
}

impl QuickView {
    pub fn open(ctx: &egui::Context, path: &Path, bytes: u64, prefs: &ViewerPrefs, has_prev: bool, has_next: bool) -> Self {
        let game = crate::engine::clip_game(path);
        let mut q = QuickView {
            path: path.to_path_buf(),
            game: if game == "Desktop" { tr!("Рабочий стол", "Desktop", "Робочий стіл").to_string() } else { game },
            file: path.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default(),
            bytes,
            player: None,
            error: None,
            tex: None,
            audio: None,
            _out: None,
            scrubbing: None,
            zoom: 1.0,
            center: vec2(0.5, 0.5),
            dragged: false,
            has_prev,
            has_next,
            shot: Arc::new(parking_lot::Mutex::new(None)),
            shot_note: None,
            menu: Menu::None,
            speed_anchor: None,
            track_anchor: None,
        };
        if !path.is_file() {
            q.error = Some(tr!("Файл не найден - возможно, его переместили или удалили", "File not found - it may have been moved or deleted", "Файл не знайдено - можливо, його перемістили або видалили").into());
            return q;
        }
        match ffmpeg::probe(path) {
            Ok(info) => {
                let out = if info.has_audio { AudioOut::new() } else { None };
                q.audio = out.as_ref().map(|a| a.shared.clone());
                q._out = out;
                if let Some(a) = &q.audio {
                    a.set_volume(if prefs.muted { 0.0 } else { prefs.volume });
                }
                // The speed carries over from clip to clip, like in JustView.
                let speed = if QUICK_SPEEDS.iter().any(|s| (*s - prefs.speed).abs() < 1e-3) { prefs.speed } else { 1.0 };
                let mut pl = Player::new(path, info, q.audio.clone(), ctx, [2560, 1600], speed, false);
                pl.set_paused(true); // opens on the first frame, a click starts it
                q.player = Some(pl);
            }
            Err(e) => q.error = Some(e),
        }
        q
    }

    /// Draws the quick view over the whole window.
    pub fn ui(&mut self, ctx: &egui::Context, prefs: &mut ViewerPrefs) -> QuickAction {
        let mut action = QuickAction::None;
        if let Some(pl) = &mut self.player {
            if let Some(dt) = pl.update() {
                ctx.request_repaint_after(Duration::from_secs_f64(dt));
            }
            if pl.frame_changed {
                if let Some(f) = &pl.current {
                    let img = egui::ImageData::Color(f.image.clone());
                    match &mut self.tex {
                        Some(t) if t.size() == f.image.size => t.set(img, TextureOptions::LINEAR),
                        _ => self.tex = Some(ctx.load_texture("quick_frame", img, TextureOptions::LINEAR)),
                    }
                }
            }
            if pl.error.is_some() && self.error.is_none() {
                self.error = pl.error.clone();
            }
        }
        let frame = egui::Frame::none().fill(color::SURFACE).inner_margin(egui::Margin::same(space::S20));
        egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
            ui.spacing_mut().item_spacing = vec2(space::S8, 0.0);
            // Header: game and file on the left, picture size and file size on the right.
            let res = self.player.as_ref().filter(|p| p.info.has_video).map(|p| format!("{}x{}", p.info.width, p.info.height));
            ui.horizontal(|ui| {
                ui.set_min_height(size::BUTTON_MD);
                ui.label(RichText::new(glyph::FILM).font(theme::icon(16.0)).color(color::TEXT));
                ui.label(RichText::new(&self.game).font(theme::headline()).color(color::TEXT));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if w::icon_button(ui, glyph::CLOSE, tr!("Закрыть  Esc", "Close  Esc", "Закрити  Esc")).clicked() {
                        action = QuickAction::Close;
                    }
                });
            });
            ui.add_space(space::S4);
            ui.horizontal(|ui| {
                let right = match &res {
                    Some(r) => format!("{r}    {}", util::format_bytes(self.bytes)),
                    None => util::format_bytes(self.bytes),
                };
                let right_w = ui.fonts(|f| f.layout_no_wrap(right.clone(), theme::caption(), color::TEXT).size().x);
                ui.allocate_ui_with_layout(vec2((ui.available_width() - right_w - space::S16).max(40.0), 18.0), Layout::left_to_right(Align::Center), |ui| {
                    ui.add(egui::Label::new(RichText::new(&self.file).font(theme::caption()).color(color::TEXT_SECONDARY)).truncate());
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(RichText::new(right).font(theme::caption()).color(color::TEXT_SECONDARY));
                });
            });
            ui.add_space(space::S12);

            let avail = ui.available_rect_before_wrap();
            let full = Rect::from_min_max(avail.min, pos2(avail.right(), avail.bottom() - CONTROLS_H - space::S12));
            let stage = full.shrink2(vec2(ARROW_COL, 0.0));
            self.stage(ui, stage);
            // Arrows left and right of the picture: previous / next clip.
            for (dir, enabled, g, name) in [
                (-1, self.has_prev, glyph::CHEVRON_LEFT, tr!("Предыдущий клип  PgUp", "Previous clip  PgUp", "Попередній кліп  PgUp")),
                (1, self.has_next, glyph::CHEVRON_RIGHT, tr!("Следующий клип  PgDn", "Next clip  PgDn", "Наступний кліп  PgDn")),
            ] {
                let cx = if dir < 0 { full.left() + ARROW_COL / 2.0 - space::S4 } else { full.right() - ARROW_COL / 2.0 + space::S4 };
                let r = Rect::from_center_size(pos2(cx, full.center().y), vec2(ARROW_D, ARROW_D));
                if side_arrow(ui, r, g, name, enabled, dir) {
                    action = QuickAction::Step(dir);
                }
            }
            let controls = Rect::from_min_max(pos2(avail.left(), stage.bottom() + space::S12), avail.max);
            if self.controls(ui, controls, prefs) {
                action = QuickAction::Delete;
            }
            self.shot_toast(ui, stage);
            self.menus(ui, controls, prefs);
        });
        if !matches!(action, QuickAction::None) {
            return action;
        }
        if let Some(step) = self.keys(ctx, prefs) {
            return QuickAction::Step(step);
        }
        action
    }

    fn stage(&mut self, ui: &mut Ui, stage: Rect) {
        let ctx = ui.ctx().clone();
        let painter = ui.painter().clone();
        let rounding = Rounding::same(radius::INPUT);
        painter.rect_filled(stage, rounding, color::CANVAS);
        let resp = ui.interact(stage, Id::new("quick_stage"), Sense::click_and_drag());
        if let Some(err) = &self.error {
            let c = stage.center();
            painter.text(c - vec2(0.0, 28.0), Align2::CENTER_CENTER, glyph::WARNING, theme::icon(28.0), color::WARNING);
            let g = ui.fonts(|f| f.layout(err.clone(), theme::body(), color::TEXT_SECONDARY, (stage.width() - 64.0).clamp(200.0, 520.0)));
            painter.galley(pos2(c.x - g.size().x / 2.0, c.y), g, color::TEXT_SECONDARY);
            return;
        }
        let Some(pl) = &mut self.player else { return };
        let mut pic = stage;
        if let (Some(tex), true) = (&self.tex, pl.has_video()) {
            let [tw, th] = tex.size();
            let s = (stage.width() / tw as f32).min(stage.height() / th as f32);
            pic = Rect::from_center_size(stage.center(), vec2(tw as f32 * s, th as f32 * s));
            // Ctrl + wheel zooms around the pointer, dragging moves the zoomed picture.
            if resp.hovered() {
                let zd = ctx.input(|i| if i.modifiers.ctrl { i.zoom_delta() } else { 1.0 });
                if (zd - 1.0).abs() > 1e-4 {
                    if let Some(p) = resp.hover_pos() {
                        let at = (p - pic.center()) / pic.size();
                        let (z, c) = zoom_at(self.zoom, self.center, vec2(at.x, at.y), self.zoom * zd);
                        self.zoom = z;
                        self.center = c;
                    }
                }
            }
            if ctx.input(|i| i.pointer.primary_pressed()) {
                self.dragged = false; // a new press: a click after a pan still toggles playback
            }
            if resp.dragged() && self.zoom > 1.0 {
                let d = resp.drag_delta();
                if d.length() > 0.0 {
                    self.dragged = true;
                    self.center = clamp_center(self.center - d / pic.size() / self.zoom, self.zoom);
                }
            }
            if resp.double_clicked() {
                self.zoom = 1.0;
                self.center = vec2(0.5, 0.5);
            }
            let half = 0.5 / self.zoom;
            let uv = Rect::from_min_max(pos2(self.center.x - half, self.center.y - half), pos2(self.center.x + half, self.center.y + half));
            egui::Image::new(egui::load::SizedTexture::new(tex.id(), pic.size())).uv(uv).rounding(rounding).paint_at(ui, pic);
            if self.zoom > 1.001 {
                let text = format!("{:.0}%", self.zoom * 100.0);
                let g = ui.fonts(|f| f.layout_no_wrap(text, theme::caption(), color::TEXT));
                let pill = Rect::from_min_size(pos2(pic.right() - g.size().x - 10.0 - space::S12, pic.top() + space::S12), g.size() + vec2(10.0, 4.0));
                painter.rect_filled(pill, Rounding::same(radius::PILL), Color32::from_black_alpha(180));
                painter.galley(pill.min + vec2(5.0, 2.0), g, color::TEXT);
            }
        } else if !pl.has_video() && pl.info.has_audio {
            painter.text(stage.center(), Align2::CENTER_CENTER, glyph::VOLUME, theme::icon(40.0), color::TEXT_SECONDARY);
        }
        // Big play mark while paused.
        let paused_t = ctx.animate_bool_with_time(Id::new("quick_paused"), pl.paused || pl.ended, w::dur(motion::HOVER));
        if paused_t > 0.0 {
            let c = pic.center();
            painter.circle_filled(c, 30.0, Color32::from_black_alpha((170.0 * paused_t) as u8));
            let g = if pl.ended { glyph::REPLAY } else { glyph::PLAY };
            painter.text(c + vec2(2.0, 0.0), Align2::CENTER_CENTER, g, theme::icon(22.0), color::TEXT.gamma_multiply(paused_t));
        }
        if self.zoom > 1.0 {
            ctx.set_cursor_icon(if resp.dragged() { CursorIcon::Grabbing } else if resp.hovered() { CursorIcon::Grab } else { CursorIcon::Default });
        }
        if resp.clicked() && !self.dragged && !resp.double_clicked() {
            pl.toggle();
        }
    }

    /// Play, seek bar, time, volume, folder and delete. True when "delete" was pressed.
    fn controls(&mut self, ui: &mut Ui, row: Rect, prefs: &mut ViewerPrefs) -> bool {
        let ctx = ui.ctx().clone();
        let mut delete = false;
        let mut shoot: Option<f64> = None;
        let path = self.path.clone();
        let audio = self.audio.clone();
        let Some(pl) = &mut self.player else {
            // No player (file missing or unreadable): only the file actions.
            ui.allocate_new_ui(egui::UiBuilder::new().max_rect(row), |ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if w::icon_button(ui, glyph::DELETE, tr!("Удалить", "Delete", "Видалити")).clicked() {
                        delete = true;
                    }
                });
            });
            return delete;
        };
        let dur = pl.duration();
        let pos = pl.position();
        let time = format!("{} / {}", fmt_time(pos), fmt_time(dur));
        let time_w = ui.fonts(|f| f.layout_no_wrap(format!("{} / {}", fmt_time(dur), fmt_time(dur)), theme::caption(), color::TEXT).size().x);
        let tracks: Vec<String> = pl.info.audio_tracks.iter().map(|t| t.label()).collect();
        let cur_track = pl.audio_track;
        let track_short = tracks.get(cur_track.min(tracks.len().saturating_sub(1))).map(|n| n.split_once(" (").map(|(a, _)| a.to_string()).unwrap_or_else(|| n.clone()));
        let pill_w = |t: &str, icon: bool| ui.fonts(|f| f.layout_no_wrap(t.to_owned(), theme::subhead(), color::TEXT).size().x + if icon { 22.0 } else { 0.0 } + space::S16 * 2.0).clamp(64.0, 200.0);
        let pills_w = pill_w(&super::viewer::fmt_speed(pl.speed), false) + space::S8 + if tracks.len() > 1 { pill_w(track_short.as_deref().unwrap_or(""), true) + space::S8 } else { 0.0 };
        let (mut speed_anchor, mut track_anchor) = (None, None);
        let open_menu = self.menu;
        let mut toggle_menu = None;
        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(row), |ui| {
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = space::S8;
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
                // Right side first, so the seek bar takes what is left.
                let right_w = time_w + space::S16 + size::BUTTON_MD * 4.0 + 96.0 + space::S8 * 6.0 + pills_w;
                let seek_w = (ui.available_width() - right_w).max(80.0);
                let (seek_r, seek) = ui.allocate_exact_size(vec2(seek_w, CONTROLS_H), Sense::click_and_drag());
                let painter = ui.painter().clone();
                let hv = ctx.animate_bool_with_time(Id::new("quick_seek_h"), seek.hovered() || seek.dragged(), w::dur(motion::HOVER));
                let track = Rect::from_center_size(seek_r.center(), vec2(seek_r.width() - 12.0, size::SLIDER_TRACK));
                painter.rect_filled(track, Rounding::same(radius::PILL), color::SLIDER_TRACK);
                let frac = if dur > 0.0 { (pos / dur).clamp(0.0, 1.0) as f32 } else { 0.0 };
                let played = Rect::from_min_max(track.min, pos2(track.left() + track.width() * frac, track.max.y));
                painter.rect_filled(played, Rounding::same(radius::PILL), color::FOCUS);
                painter.circle_filled(pos2(played.max.x, track.center().y), 6.0 + 1.5 * hv, color::FOCUS);
                let t_at = |x: f32| ((x - track.left()) / track.width()).clamp(0.0, 1.0) as f64 * dur;
                if seek.hovered() {
                    ctx.set_cursor_icon(CursorIcon::PointingHand);
                    if let Some(p) = seek.hover_pos() {
                        let g = ui.fonts(|f| f.layout_no_wrap(fmt_time(t_at(p.x)), theme::caption(), color::TEXT));
                        let tr = Rect::from_center_size(pos2(p.x, seek_r.top() - space::S8), g.size() + vec2(16.0, 8.0));
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
                ui.add_space(space::S8);
                ui.allocate_ui_with_layout(vec2(time_w, CONTROLS_H), Layout::left_to_right(Align::Center), |ui| {
                    ui.label(RichText::new(time).font(theme::caption()).color(color::TEXT));
                });
                if pl.info.has_audio {
                    let vg = if prefs.muted || prefs.volume <= 0.001 { glyph::MUTE } else { glyph::VOLUME };
                    if w::icon_button(ui, vg, if prefs.muted { tr!("Включить звук  M", "Unmute  M", "Увімкнути звук  M") } else { tr!("Выключить звук  M", "Mute  M", "Вимкнути звук  M") }).clicked() {
                        prefs.muted = !prefs.muted;
                    }
                    let mut v = if prefs.muted { 0.0 } else { prefs.volume };
                    if w::slider(ui, "quick_volume", &mut v, 0.0, 1.0, 0.01, 96.0).on_hover_text(trf!(["Громкость {:.0} %", "Volume {:.0} %", "Гучність {:.0} %"], v * 100.0)).changed() {
                        prefs.volume = v;
                        prefs.muted = v <= 0.001;
                    }
                    if let Some(a) = &audio {
                        a.set_volume(if prefs.muted { 0.0 } else { prefs.volume });
                    }
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if w::icon_button(ui, glyph::DELETE, tr!("Удалить", "Delete", "Видалити")).clicked() {
                        delete = true;
                    }
                    if w::icon_button(ui, glyph::FOLDER, tr!("Открыть расположение", "Show in folder", "Відкрити розташування")).clicked() {
                        util::open_in_explorer(&path, true);
                    }
                    if pl.has_video() && w::icon_button(ui, glyph::CAMERA, tr!("Скриншот кадра  S", "Frame screenshot  S", "Скриншот кадру  S")).clicked() {
                        shoot = Some(pl.position());
                    }
                    // Speed and audio track, the same pills as in JustView.
                    let non_default = (pl.speed - 1.0).abs() > 1e-3;
                    let mut r = super::viewer::menu_pill(ui, "quick_speed", None, &super::viewer::fmt_speed(pl.speed), non_default, open_menu == Menu::Speed);
                    if open_menu == Menu::None {
                        r = r.on_hover_text(tr!("Скорость  [ ]", "Speed  [ ]", "Швидкість  [ ]")); // no tip over an open menu
                    }
                    if r.clicked() {
                        toggle_menu = Some(Menu::Speed);
                    }
                    speed_anchor = Some(r.rect);
                    if tracks.len() > 1 {
                        let full_name = &tracks[cur_track.min(tracks.len() - 1)];
                        let mut r = super::viewer::menu_pill(ui, "quick_track", Some(glyph::AUDIO), track_short.as_deref().unwrap_or(""), false, open_menu == Menu::Track);
                        if open_menu == Menu::None {
                            r = r.on_hover_text(trf!(["Звуковая дорожка: {full_name}  A", "Audio track: {full_name}  A", "Звукова доріжка: {full_name}  A"]));
                        }
                        if r.clicked() {
                            toggle_menu = Some(Menu::Track);
                        }
                        track_anchor = Some(r.rect);
                    }
                });
            });
        });
        self.speed_anchor = speed_anchor;
        self.track_anchor = track_anchor;
        if let Some(m) = toggle_menu {
            self.menu = if self.menu == m { Menu::None } else { m };
        }
        if let Some(at) = shoot {
            self.shoot_frame(at);
        }
        delete
    }

    /// The open speed or track menu above its pill (JustView's menu: rows, check on the current).
    fn menus(&mut self, ui: &mut Ui, controls: Rect, prefs: &mut ViewerPrefs) {
        let ctx = ui.ctx().clone();
        let Some(pl) = &mut self.player else { return };
        let (anchor, items, selected): (Option<Rect>, Vec<String>, usize) = match self.menu {
            Menu::None => return,
            Menu::Speed => (
                self.speed_anchor,
                QUICK_SPEEDS.iter().rev().map(|s| if *s == 1.0 { tr!("1× обычная", "1× normal", "1× звичайна").to_string() } else { super::viewer::fmt_speed(*s) }).collect(),
                QUICK_SPEEDS.iter().rev().position(|s| (*s - pl.speed).abs() < 1e-3).unwrap_or(usize::MAX),
            ),
            Menu::Track => (self.track_anchor, pl.info.audio_tracks.iter().map(|t| t.label()).collect(), pl.audio_track),
        };
        let Some(anchor) = anchor else {
            self.menu = Menu::None;
            return;
        };
        let widest = items.iter().map(|t| ui.fonts(|f| f.layout_no_wrap(t.clone(), theme::subhead(), color::TEXT).size().x)).fold(0.0f32, f32::max);
        let mw = (widest + space::S32 + space::S24).clamp(140.0, 360.0);
        let mh = MENU_ROW * items.len() as f32 + space::S8 * 2.0;
        let open_t = ctx.animate_bool_with_time(Id::new(("quick_menu", self.menu as u8)), true, w::dur(motion::INDICATOR));
        let lift = (1.0 - open_t) * 8.0;
        let left = (anchor.center().x - mw / 2.0).min(controls.right() - mw).max(controls.left());
        let mr = Rect::from_min_size(pos2(left, anchor.top() - space::S8 - mh + lift), vec2(mw, mh));
        let mut chosen = None;
        egui::Area::new(Id::new("quick_menu_area")).order(egui::Order::Foreground).fixed_pos(mr.min).show(&ctx, |ui| {
            ui.set_opacity(open_t);
            let (rect, _) = ui.allocate_exact_size(mr.size(), Sense::click());
            let p = ui.painter();
            p.rect_filled(rect, Rounding::same(radius::GROUP), color::SURFACE_RAISED);
            for (i, name) in items.iter().enumerate() {
                let r = Rect::from_min_size(pos2(rect.left() + space::S4, rect.top() + space::S8 + i as f32 * MENU_ROW), vec2(rect.width() - space::S8, MENU_ROW));
                let id = Id::new(("quick_menu_row", self.menu as u8, i));
                let rr = ui.interact(r, id, Sense::click());
                let h = ctx.animate_bool_with_time(id.with("h"), rr.hovered(), w::dur(motion::HOVER));
                if h > 0.0 {
                    p.rect_filled(r, Rounding::same(radius::INPUT), color::SURFACE_HOVER.gamma_multiply(h));
                }
                let sel = i == selected;
                if sel {
                    let c = pos2(r.left() + space::S16, r.center().y);
                    p.add(egui::Shape::line(vec![c + vec2(-4.0, 0.0), c + vec2(-1.0, 3.5), c + vec2(5.0, -4.0)], egui::Stroke::new(1.5_f32, color::FOCUS)));
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
                    let s = QUICK_SPEEDS[QUICK_SPEEDS.len() - 1 - i];
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

    /// True while a speed / track menu is open (Esc closes it first).
    pub fn close_menu(&mut self) -> bool {
        let open = self.menu != Menu::None;
        self.menu = Menu::None;
        open
    }

    /// Saves the frame at `at` seconds as PNG at the clip's full size (FFmpeg reads it from the
    /// file, so it is sharper than the picture on screen), next to the clip.
    fn shoot_frame(&mut self, at: f64) {
        let Some(ff) = ffmpeg::ffmpeg() else {
            *self.shot.lock() = Some(Err(ffmpeg::missing().into()));
            return;
        };
        let dir = self.path.parent().map(Path::to_path_buf).unwrap_or_default();
        let game = util::sanitize_file_name(&crate::engine::clip_game(&self.path));
        let base = format!("{game} - Screenshot {}", util::timestamp_for_file());
        let mut out = dir.join(format!("{base}.png"));
        let mut i = 2;
        while out.exists() {
            out = dir.join(format!("{base} ({i}).png"));
            i += 1;
        }
        let (src, slot) = (self.path.clone(), self.shot.clone());
        std::thread::spawn(move || {
            let r = ffmpeg::hidden_command(ff)
                .args(["-v", "error", "-y", "-ss", &format!("{at:.3}"), "-i"])
                .arg(&src)
                .args(["-frames:v", "1", "-update", "1"])
                .arg(&out)
                .output();
            *slot.lock() = Some(match r {
                Ok(o) if o.status.success() && out.is_file() => Ok(out),
                Ok(o) => Err(String::from_utf8_lossy(&o.stderr).trim().to_string()),
                Err(e) => Err(e.to_string()),
            });
        });
    }

    /// "Screenshot saved" note over the picture for a couple of seconds.
    fn shot_toast(&mut self, ui: &mut Ui, stage: Rect) {
        if let Some(r) = self.shot.lock().take() {
            self.shot_note = Some((r, std::time::Instant::now()));
        }
        let Some((r, at)) = &self.shot_note else { return };
        let age = at.elapsed().as_secs_f32();
        if age > 2.6 {
            self.shot_note = None;
            return;
        }
        ui.ctx().request_repaint_after(Duration::from_millis(50));
        let alpha = (1.0 - ((age - 2.2) / 0.4).clamp(0.0, 1.0)) * (age / 0.15).clamp(0.0, 1.0);
        let (g, text, col) = match r {
            Ok(p) => (glyph::CHECK, trf!(["Скриншот сохранён: {}", "Screenshot saved: {}", "Скриншот збережено: {}"], p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()), color::SUCCESS),
            Err(e) => (glyph::WARNING, trf!(["Не удалось сделать скриншот: {}", "Could not take a screenshot: {}", "Не вдалося зробити скриншот: {}"], e), color::WARNING),
        };
        let galley = ui.fonts(|f| f.layout_no_wrap(text, theme::subhead(), color::TEXT));
        let sz = vec2(galley.size().x + 22.0 + space::S16 * 2.0, size::BUTTON_MD + 4.0);
        let pill = Rect::from_center_size(pos2(stage.center().x, stage.bottom() - space::S16 - sz.y / 2.0), sz);
        let p = ui.painter();
        p.rect_filled(pill, Rounding::same(radius::PILL), Color32::from_black_alpha((200.0 * alpha) as u8));
        p.text(pos2(pill.left() + space::S16, pill.center().y), Align2::LEFT_CENTER, g, theme::icon(13.0), col.gamma_multiply(alpha));
        p.galley(pos2(pill.left() + space::S16 + 22.0, pill.center().y - galley.size().y / 2.0), galley, color::TEXT.gamma_multiply(alpha));
    }

    /// Player keys; returns a clip step for PageUp / PageDown.
    fn keys(&mut self, ctx: &egui::Context, prefs: &mut ViewerPrefs) -> Option<i32> {
        if ctx.memory(|m| m.focused().is_some()) {
            return None;
        }
        let pressed = |k: Key| ctx.input(|i| i.key_pressed(k));
        if pressed(Key::PageUp) && self.has_prev {
            return Some(-1);
        }
        if pressed(Key::PageDown) && self.has_next {
            return Some(1);
        }
        let Some(pl) = &mut self.player else { return None };
        if pressed(Key::S) && pl.has_video() {
            let at = pl.position();
            self.shoot_frame(at);
            return None;
        }
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
        let step = |dir: i32, cur: f32| {
            let i = QUICK_SPEEDS.iter().position(|s| (*s - cur).abs() < 1e-3).unwrap_or(2) as i32;
            QUICK_SPEEDS[(i + dir).clamp(0, QUICK_SPEEDS.len() as i32 - 1) as usize]
        };
        if pressed(Key::OpenBracket) || pressed(Key::CloseBracket) || pressed(Key::Backspace) {
            let s = if pressed(Key::Backspace) { 1.0 } else { step(if pressed(Key::OpenBracket) { -1 } else { 1 }, pl.speed) };
            pl.set_speed(s);
            prefs.speed = s;
        }
        if pressed(Key::A) && pl.info.audio_tracks.len() > 1 {
            let t = (pl.audio_track + 1) % pl.info.audio_tracks.len();
            pl.set_audio_track(t);
        }
        if pressed(Key::Comma) {
            pl.step_back();
        }
        if pressed(Key::Period) {
            pl.step_forward();
        }
        if pressed(Key::Num0) {
            self.zoom = 1.0;
            self.center = vec2(0.5, 0.5);
        }
        if pressed(Key::M) {
            prefs.muted = !prefs.muted;
            if let Some(a) = &self.audio {
                a.set_volume(if prefs.muted { 0.0 } else { prefs.volume });
            }
        }
        None
    }
}

/// Round arrow button beside the picture; fades out at the ends of the list.
fn side_arrow(ui: &mut Ui, r: Rect, g: &str, name: &str, enabled: bool, dir: i32) -> bool {
    let id = Id::new(("quick_arrow", dir));
    let t = ui.ctx().animate_bool_with_time(id.with("on"), enabled, w::dur(motion::HOVER));
    if t <= 0.0 {
        return false;
    }
    let resp = ui.interact(r, id, if enabled { Sense::click() } else { Sense::hover() });
    let resp = if enabled { resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text(name) } else { resp };
    let h = ui.ctx().animate_bool_with_time(id.with("h"), resp.hovered() && enabled, w::dur(motion::HOVER));
    let down = resp.is_pointer_button_down_on();
    let p = ui.ctx().animate_bool_with_time(id.with("p"), down, w::dur(if down { motion::PRESS_DOWN } else { motion::PRESS_RELEASE }));
    let rr = Rect::from_center_size(r.center(), r.size() * (1.0 - (1.0 - motion::PRESS_SCALE) * p));
    let fill = w::lerp_color(color::SURFACE_RAISED, color::SURFACE_HOVER, h);
    let fill = w::lerp_color(fill, color::SURFACE_PRESSED, p);
    let painter = ui.painter();
    painter.circle_filled(rr.center(), rr.width() / 2.0, fill.gamma_multiply(t));
    painter.text(rr.center(), Align2::CENTER_CENTER, g, theme::icon(16.0), color::TEXT.gamma_multiply(t));
    enabled && resp.clicked()
}

/// New zoom and centre so that the picture point under the pointer stays under it. `at` is the
/// pointer offset from the middle of the picture box in box sizes (-0.5..0.5).
pub fn zoom_at(zoom: f32, center: Vec2, at: Vec2, new_zoom: f32) -> (f32, Vec2) {
    let nz = new_zoom.clamp(1.0, MAX_ZOOM);
    let point = center + at / zoom;
    (nz, clamp_center(point - at / nz, nz))
}

/// Keeps the visible part inside the picture.
pub fn clamp_center(c: Vec2, zoom: f32) -> Vec2 {
    let half = 0.5 / zoom.max(1.0);
    vec2(c.x.clamp(half, 1.0 - half), c.y.clamp(half, 1.0 - half))
}

fn fmt_time(t: f64) -> String {
    let t = t.max(0.0) as u64;
    let (h, m, s) = (t / 3600, t / 60 % 60, t % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zoom_keeps_the_point_under_the_pointer() {
        // Zoom 2x at the right quarter of the picture: that point stays where it was.
        let at = vec2(0.25, 0.0);
        let (z, c) = zoom_at(1.0, vec2(0.5, 0.5), at, 2.0);
        assert_eq!(z, 2.0);
        let before = 0.5 + 0.25;
        let after = c.x + at.x / z;
        assert!((before - after).abs() < 1e-5);
    }

    #[test]
    fn zoom_is_limited_and_stays_inside() {
        let (z, c) = zoom_at(1.0, vec2(0.5, 0.5), vec2(0.5, 0.5), 100.0);
        assert_eq!(z, MAX_ZOOM);
        assert!(c.x <= 1.0 - 0.5 / MAX_ZOOM + 1e-6 && c.y <= 1.0 - 0.5 / MAX_ZOOM + 1e-6);
        let (z, c) = zoom_at(4.0, vec2(0.9, 0.1), vec2(0.0, 0.0), 0.5);
        assert_eq!(z, 1.0);
        assert_eq!(c, vec2(0.5, 0.5));
    }

    #[test]
    fn time_is_two_digit() {
        assert_eq!(fmt_time(0.0), "00:00");
        assert_eq!(fmt_time(120.4), "02:00");
        assert_eq!(fmt_time(3725.0), "1:02:05");
    }
}

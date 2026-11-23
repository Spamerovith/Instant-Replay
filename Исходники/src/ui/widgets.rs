//! Custom controls with built-in motion (press compress + fill step, hover fades, sliding
//! indicators). All values come from `theme`.

use std::sync::atomic::{AtomicBool, Ordering};

use egui::{pos2, vec2, Align, Align2, Color32, CursorIcon, Id, Layout, Rect, Response, Rounding, Sense, Stroke, Ui};

use super::theme::{self, color, motion, radius, size, space};

static REDUCED: AtomicBool = AtomicBool::new(false);

pub fn set_reduced_motion(v: bool) {
    REDUCED.store(v, Ordering::Relaxed);
}

/// Duration scaled by the reduced-motion preference.
pub fn dur(d: f32) -> f32 {
    if REDUCED.load(Ordering::Relaxed) {
        0.0
    } else {
        d
    }
}

fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3)
}

pub fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgba_premultiplied(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()), l(a.a(), b.a()))
}

/// Press progress 0..1 (fast down, slower release) and hover progress 0..1.
fn interaction(ui: &Ui, resp: &Response) -> (f32, f32) {
    let pressed = resp.is_pointer_button_down_on() || (resp.has_focus() && ui.input(|i| i.key_down(egui::Key::Space) || i.key_down(egui::Key::Enter)));
    let p = ui.ctx().animate_value_with_time(
        resp.id.with("press"),
        if pressed { 1.0 } else { 0.0 },
        dur(if pressed { motion::PRESS_DOWN } else { motion::PRESS_RELEASE }),
    );
    let h = ui.ctx().animate_bool_with_time(resp.id.with("hover"), resp.hovered(), dur(motion::HOVER));
    (ease_out(p), h)
}

fn press_rect(rect: Rect, p: f32) -> Rect {
    let s = 1.0 - (1.0 - motion::PRESS_SCALE) * p;
    Rect::from_center_size(rect.center(), rect.size() * s)
}

fn focus_ring(ui: &Ui, resp: &Response, rect: Rect, rounding: f32) {
    if resp.has_focus() {
        ui.painter().rect_stroke(rect.expand(3.0), Rounding::same(rounding + 3.0), Stroke::new(2.0_f32, color::FOCUS));
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    /// The single filled action of a screen.
    Primary,
    /// Outlined pill.
    Ghost,
    /// Ghost pill whose label carries the record colour (stop recording).
    Record,
}

pub fn pill_button(ui: &mut Ui, text: &str, kind: Kind, height: f32, enabled: bool) -> Response {
    let font = theme::body();
    let galley = ui.fonts(|f| f.layout_no_wrap(text.to_owned(), font.clone(), Color32::PLACEHOLDER));
    let pad = if height >= size::BUTTON_LG { 22.0 } else { space::S16 };
    let lead = if kind == Kind::Record { 14.0 } else { 0.0 };
    let desired = vec2(galley.size().x + pad * 2.0 + lead, height);
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(desired, sense);
    let resp = resp.on_hover_cursor(if enabled { CursorIcon::PointingHand } else { CursorIcon::NotAllowed });
    let (p, h) = interaction(ui, &resp);
    let r = press_rect(rect, p);
    let rounding = Rounding::same(radius::PILL);
    let painter = ui.painter();
    let (fill, label, stroke) = match kind {
        Kind::Primary => {
            let base = if enabled { color::ACTION } else { color::SURFACE_PRESSED };
            let fill = lerp_color(base, color::ACTION_PRESSED, if enabled { (h * 0.35).max(p) } else { 0.0 });
            (fill, if enabled { color::ACTION_LABEL } else { color::TEXT_DISABLED }, Stroke::NONE)
        }
        Kind::Ghost | Kind::Record => {
            let fill = lerp_color(Color32::TRANSPARENT, color::SURFACE_HOVER, h);
            let fill = lerp_color(fill, color::SURFACE_PRESSED, p);
            let label = if !enabled {
                color::TEXT_DISABLED
            } else if kind == Kind::Record {
                color::RECORD
            } else {
                color::TEXT
            };
            (fill, label, Stroke::new(1.0_f32, if enabled { color::FIELD_BORDER } else { color::SURFACE_PRESSED }))
        }
    };
    painter.rect(r, rounding, fill, stroke);
    let text_pos = pos2(r.center().x - galley.size().x / 2.0 + lead / 2.0, r.center().y - galley.size().y / 2.0);
    if kind == Kind::Record {
        painter.rect_filled(
            Rect::from_center_size(pos2(text_pos.x - 12.0, r.center().y), vec2(9.0, 9.0)),
            Rounding::same(2.0),
            color::RECORD,
        );
    }
    painter.galley(text_pos, galley, label);
    focus_ring(ui, &resp, rect, radius::PILL);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, text));
    resp
}

/// Text link with a chevron, e.g. "Открыть папку ›".
pub fn link(ui: &mut Ui, text: &str) -> Response {
    let font = theme::subhead();
    let label = format!("{text}  ›");
    let galley = ui.fonts(|f| f.layout_no_wrap(label.clone(), font, Color32::PLACEHOLDER));
    let (rect, resp) = ui.allocate_exact_size(galley.size() + vec2(0.0, 4.0), Sense::click());
    let resp = resp.on_hover_cursor(CursorIcon::PointingHand);
    let (p, h) = interaction(ui, &resp);
    let c = lerp_color(color::FOCUS, color::TEXT, h * 0.25);
    let c = c.gamma_multiply(1.0 - 0.25 * p);
    ui.painter().galley(pos2(rect.left(), rect.top() + 2.0), galley, c);
    focus_ring(ui, &resp, rect, 4.0);
    resp
}

/// Round icon-only button (with a mandatory accessible name / tooltip).
pub fn icon_button(ui: &mut Ui, glyph: &str, name: &str) -> Response {
    let d = size::BUTTON_MD;
    let (rect, resp) = ui.allocate_exact_size(vec2(d, d), Sense::click());
    let resp = resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text(name);
    let (p, h) = interaction(ui, &resp);
    let r = press_rect(rect, p);
    let fill = lerp_color(Color32::TRANSPARENT, color::SURFACE_HOVER, h);
    let fill = lerp_color(fill, color::SURFACE_PRESSED, p);
    ui.painter().rect_filled(r, Rounding::same(radius::PILL), fill);
    ui.painter().text(r.center(), Align2::CENTER_CENTER, glyph, theme::icon(14.0), color::TEXT_SECONDARY);
    focus_ring(ui, &resp, rect, radius::PILL);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, name));
    resp
}

pub fn switch(ui: &mut Ui, on: &mut bool, name: &str) -> Response {
    let (rect, mut resp) = ui.allocate_exact_size(vec2(size::SWITCH_W, size::SWITCH_H), Sense::click());
    resp = resp.on_hover_cursor(CursorIcon::PointingHand);
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let t = ease_out(ui.ctx().animate_bool_with_time(resp.id, *on, dur(motion::SWITCH)));
    let (p, h) = interaction(ui, &resp);
    let off_fill = lerp_color(color::SLIDER_TRACK, color::TRACK_HOVER, h);
    let track = lerp_color(off_fill, color::FOCUS, t);
    ui.painter().rect_filled(rect, Rounding::same(radius::PILL), track);
    let travel = size::SWITCH_W - size::SWITCH_H;
    let cx = rect.left() + size::SWITCH_H / 2.0 + travel * t;
    // Thumb stretches slightly while pressed (tactile feedback).
    let thumb_w = size::SWITCH_THUMB + 4.0 * p;
    let thumb = Rect::from_center_size(pos2(cx, rect.center().y), vec2(thumb_w, size::SWITCH_THUMB));
    ui.painter().rect_filled(thumb, Rounding::same(radius::PILL), color::TEXT);
    focus_ring(ui, &resp, rect, radius::PILL);
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, *on, name));
    resp
}

/// Segmented control with a sliding indicator. Returns true when the selection changed.
pub fn segmented(ui: &mut Ui, id: &str, options: &[(&str, bool)], selected: &mut usize) -> bool {
    let font = theme::subhead();
    let seg_pad = space::S12;
    let widths: Vec<f32> = options
        .iter()
        .map(|(t, _)| ui.fonts(|f| f.layout_no_wrap(t.to_string(), theme::subhead_strong(), color::TEXT).size().x) + seg_pad * 2.0)
        .collect();
    let seg_w = widths.iter().cloned().fold(56.0, f32::max);
    let total = vec2(seg_w * options.len() as f32 + 6.0, size::SEGMENTED_H);
    let (rect, _) = ui.allocate_exact_size(total, Sense::hover());
    let painter = ui.painter().clone();
    painter.rect_filled(rect, Rounding::same(radius::INPUT), color::FIELD);
    let inner = rect.shrink(3.0);
    let base_id = Id::new(("segmented", id));
    let target_x = inner.left() + seg_w * (*selected).min(options.len().saturating_sub(1)) as f32;
    let x = ui.ctx().animate_value_with_time(base_id.with("ind"), target_x, dur(motion::INDICATOR));
    let ind = Rect::from_min_size(pos2(x, inner.top()), vec2(seg_w, inner.height()));
    painter.rect_filled(ind, Rounding::same(radius::INPUT - 3.0), color::SEGMENT_SELECTED);
    let mut changed = false;
    for (i, (label, enabled)) in options.iter().enumerate() {
        let r = Rect::from_min_size(pos2(inner.left() + seg_w * i as f32, inner.top()), vec2(seg_w, inner.height()));
        let resp = ui.interact(r, base_id.with(i), if *enabled { Sense::click() } else { Sense::hover() });
        let resp = if *enabled { resp.on_hover_cursor(CursorIcon::PointingHand) } else { resp.on_hover_text(tr!("Не поддерживается вашей видеокартой", "Not supported by your graphics card", "Не підтримується вашою відеокартою")) };
        let (p, h) = interaction(ui, &resp);
        if i != *selected && *enabled {
            let hr = press_rect(r, p);
            painter.rect_filled(hr, Rounding::same(radius::INPUT - 3.0), lerp_color(Color32::TRANSPARENT, color::FIELD_HOVER, h.max(p)));
        }
        if resp.clicked() && *enabled && i != *selected {
            *selected = i;
            changed = true;
        }
        let (f, c) = if i == *selected {
            (theme::subhead_strong(), color::TEXT)
        } else if *enabled {
            (font.clone(), color::TEXT_SECONDARY)
        } else {
            (font.clone(), color::TEXT_DISABLED)
        };
        painter.text(r.center(), Align2::CENTER_CENTER, *label, f, c);
        focus_ring(ui, &resp, r, radius::INPUT - 3.0);
        resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::RadioButton, *enabled, i == *selected, *label));
    }
    // Keyboard: arrows move the selection when any segment has focus.
    changed
}

/// Horizontal slider. `format` renders the value label shown to the right.
pub fn slider(ui: &mut Ui, id: &str, value: &mut f32, min: f32, max: f32, step: f32, width: f32) -> Response {
    let h = size::SLIDER_THUMB + 8.0;
    let (rect, mut resp) = ui.allocate_exact_size(vec2(width, h), Sense::click_and_drag());
    resp = resp.on_hover_cursor(CursorIcon::PointingHand);
    let id = Id::new(("slider", id));
    let pad = size::SLIDER_THUMB / 2.0;
    let track = Rect::from_min_max(pos2(rect.left() + pad, rect.center().y - size::SLIDER_TRACK / 2.0), pos2(rect.right() - pad, rect.center().y + size::SLIDER_TRACK / 2.0));
    let old = *value;
    if let Some(pos) = resp.interact_pointer_pos() {
        if resp.dragged() || resp.clicked() || resp.is_pointer_button_down_on() {
            let t = ((pos.x - track.left()) / track.width()).clamp(0.0, 1.0);
            *value = min + t * (max - min);
        }
    }
    if resp.has_focus() {
        let (l, r) = ui.input(|i| (i.key_pressed(egui::Key::ArrowLeft), i.key_pressed(egui::Key::ArrowRight)));
        if l {
            *value -= step;
        }
        if r {
            *value += step;
        }
    }
    *value = ((*value / step).round() * step).clamp(min, max);
    if (*value - old).abs() > f32::EPSILON {
        resp.mark_changed();
    }
    let t = (*value - min) / (max - min).max(f32::EPSILON);
    let shown = ui.ctx().animate_value_with_time(id.with("v"), t, dur(0.06));
    let painter = ui.painter();
    painter.rect_filled(track, Rounding::same(radius::PILL), color::SLIDER_TRACK);
    let fill = Rect::from_min_max(track.min, pos2(track.left() + track.width() * shown, track.max.y));
    painter.rect_filled(fill, Rounding::same(radius::PILL), color::TEXT);
    let (p, hv) = interaction(ui, &resp);
    let thumb_r = size::SLIDER_THUMB / 2.0 * (1.0 + 0.08 * hv - 0.06 * p);
    let c = pos2(track.left() + track.width() * shown, track.center().y);
    painter.circle_filled(c + vec2(0.0, 1.0), thumb_r + 0.5, Color32::from_black_alpha(90));
    painter.circle_filled(c, thumb_r, color::TEXT);
    if resp.has_focus() {
        painter.circle_stroke(c, thumb_r + 3.0, Stroke::new(2.0_f32, color::FOCUS));
    }
    resp
}

#[derive(Clone, Copy, PartialEq)]
pub enum KeycapState {
    Idle,
    Listening,
    Conflict,
}

pub fn keycap(ui: &mut Ui, text: &str, state: KeycapState) -> Response {
    let font = theme::subhead();
    let galley = ui.fonts(|f| f.layout_no_wrap(text.to_owned(), font, Color32::PLACEHOLDER));
    let w = (galley.size().x + space::S24).max(size::KEYCAP_W);
    let (rect, resp) = ui.allocate_exact_size(vec2(w, size::KEYCAP_H), Sense::click());
    let resp = resp.on_hover_cursor(CursorIcon::PointingHand);
    let (p, h) = interaction(ui, &resp);
    let r = press_rect(rect, p);
    let fill = lerp_color(color::FIELD, color::FIELD_HOVER, h);
    let fill = lerp_color(fill, color::FIELD_PRESSED, p);
    let stroke = match state {
        KeycapState::Idle => Stroke::new(1.0_f32, color::HAIRLINE),
        KeycapState::Listening => Stroke::new(2.0_f32, color::FOCUS),
        KeycapState::Conflict => Stroke::new(1.0_f32, color::RECORD),
    };
    ui.painter().rect(r, Rounding::same(radius::INPUT), fill, stroke);
    let c = match state {
        KeycapState::Listening => color::FOCUS,
        _ => color::TEXT,
    };
    ui.painter().galley(r.center() - galley.size() / 2.0, galley, c);
    focus_ring(ui, &resp, rect, radius::INPUT);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, text));
    resp
}

// ------------------------------------------------------------------ groups & rows

/// A grouped list on `surface` with an optional title above it.
pub fn group(ui: &mut Ui, title: Option<&str>, add: impl FnOnce(&mut GroupUi)) {
    if let Some(t) = title {
        ui.add_space(0.0);
        ui.horizontal(|ui| {
            ui.add_space(space::S4);
            ui.label(egui::RichText::new(t).font(theme::headline()).color(color::TEXT));
        });
        ui.add_space(space::S8 - ui.spacing().item_spacing.y);
    }
    egui::Frame::none()
        .fill(color::CARD)
        .stroke(Stroke::new(1.0_f32, color::CARD_BORDER))
        .rounding(Rounding::same(radius::GROUP))
        .inner_margin(egui::Margin::symmetric(space::S16, 0.0))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            let mut g = GroupUi { ui, first: true };
            add(&mut g);
        });
}

pub struct GroupUi<'a> {
    pub ui: &'a mut Ui,
    first: bool,
}

impl GroupUi<'_> {
    fn separator(&mut self) {
        if !self.first {
            let r = self.ui.available_rect_before_wrap();
            let y = r.top();
            self.ui.painter().hline(r.left()..=r.right() + space::S16, y, Stroke::new(1.0_f32, color::HAIRLINE));
        }
        self.first = false;
    }

    /// Label on the left keyline, control on the right keyline.
    pub fn row(&mut self, label: &str, sub: Option<&str>, control: impl FnOnce(&mut Ui)) {
        self.row_rich(label, sub, None, control);
    }

    pub fn row_rich(&mut self, label: &str, sub: Option<&str>, sub_color: Option<Color32>, control: impl FnOnce(&mut Ui)) {
        self.separator();
        let w = self.ui.available_width();
        let min_h = if sub.is_some() { size::ROW_MIN + 14.0 } else { size::ROW_MIN };
        self.ui.allocate_ui_with_layout(vec2(w, min_h), Layout::right_to_left(Align::Center), |ui| {
            ui.set_min_height(min_h);
            let row = ui.max_rect();
            ui.spacing_mut().item_spacing = vec2(space::S8, 0.0);
            // Controls first (right keyline), then the label block fills what is left,
            // vertically centred as one unit on the row's centre line.
            control(ui);
            let free = ui.available_rect_before_wrap();
            let max_w = (free.right() - row.left() - space::S16).max(40.0);
            let label_g = single_line(ui, label, theme::body(), max_w);
            let sub_g = sub.map(|s| single_line(ui, s, theme::caption(), max_w));
            let gap = 2.0;
            let block_h = label_g.size().y + sub_g.as_ref().map(|g| gap + g.size().y).unwrap_or(0.0);
            let y0 = (row.center().y - block_h / 2.0).round();
            let label_h = label_g.size().y;
            ui.painter().galley(pos2(row.left(), y0), label_g, color::TEXT);
            if let Some(g) = sub_g {
                ui.painter().galley(pos2(row.left(), y0 + label_h + gap), g, sub_color.unwrap_or(color::TEXT_SECONDARY));
            }
        });
    }

    /// Full-width custom content row.
    pub fn custom(&mut self, min_h: f32, add: impl FnOnce(&mut Ui)) {
        self.separator();
        let w = self.ui.available_width();
        self.ui.allocate_ui_with_layout(vec2(w, min_h), Layout::left_to_right(Align::Center), |ui| {
            ui.set_min_height(min_h);
            add(ui);
        });
    }
}

fn single_line(ui: &Ui, text: &str, font: egui::FontId, max_w: f32) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_owned(), font, Color32::PLACEHOLDER);
    job.wrap = egui::text::TextWrapping { max_width: max_w, max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
    ui.fonts(|f| f.layout_job(job))
}

/// Value text for the right side of a row.
pub fn value_text(ui: &mut Ui, text: &str) {
    ui.label(egui::RichText::new(text).font(theme::subhead()).color(color::TEXT_SECONDARY));
}

pub fn nav_item(ui: &mut Ui, glyph: &str, text: &str, selected: bool) -> Response {
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(vec2(w, size::NAV_ITEM), Sense::click());
    let resp = resp.on_hover_cursor(CursorIcon::PointingHand);
    let (p, h) = interaction(ui, &resp);
    if !selected {
        let fill = lerp_color(Color32::TRANSPARENT, color::SURFACE, h.max(p));
        ui.painter().rect_filled(press_rect(rect, p * 0.5), Rounding::same(radius::INPUT), fill);
    }
    let c = if selected { color::TEXT } else { lerp_color(color::TEXT_SECONDARY, color::TEXT, h) };
    ui.painter().text(pos2(rect.left() + space::S12 + 8.0, rect.center().y), Align2::CENTER_CENTER, glyph, theme::icon(16.0), c);
    ui.painter().text(pos2(rect.left() + space::S40, rect.center().y), Align2::LEFT_CENTER, text, if selected { theme::body_strong() } else { theme::body() }, c);
    focus_ring(ui, &resp, rect, radius::INPUT);
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, text));
    resp
}

/// Thin progress track (the "tape").
pub fn tape(ui: &mut Ui, id: &str, fraction: f32, width: f32, fill_color: Color32) {
    let (rect, _) = ui.allocate_exact_size(vec2(width, 4.0), Sense::hover());
    let f = ui.ctx().animate_value_with_time(Id::new(("tape", id)), fraction.clamp(0.0, 1.0), dur(motion::VALUE));
    ui.painter().rect_filled(rect, Rounding::same(radius::PILL), color::SLIDER_TRACK);
    if f > 0.0 {
        let r = Rect::from_min_size(rect.min, vec2((rect.width() * f).max(4.0), rect.height()));
        ui.painter().rect_filled(r, Rounding::same(radius::PILL), fill_color);
    }
}


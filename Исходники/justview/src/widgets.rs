//! Иконки (тонкая линия 1.5 px, сетка 24) и контролы с обратной связью нажатия.

use egui::epaint::PathShape;
use egui::{Align2, Color32, Id, Pos2, Rect, Response, Sense, Shape, Stroke, Ui, Vec2, pos2, vec2};

use crate::theme::*;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[allow(dead_code)]
pub enum Icon {
    ChevronLeft,
    ChevronRight,
    ChevronUp,
    ChevronDown,
    Play,
    Pause,
    ZoomIn,
    ZoomOut,
    Rotate,
    Info,
    Trash,
    Fullscreen,
    ExitFullscreen,
    Volume,
    VolumeLow,
    Mute,
    Loop,
    Folder,
    Copy,
    StepBack,
    StepFwd,
    Close,
    Replay,
    OpenWith,
}

struct G {
    c: Pos2,
    k: f32,
}
impl G {
    fn p(&self, x: f32, y: f32) -> Pos2 {
        self.c + vec2(x - 12.0, y - 12.0) * self.k
    }
}

fn poly(painter: &egui::Painter, g: &G, pts: &[(f32, f32)], closed: bool, stroke: Stroke) {
    let points: Vec<Pos2> = pts.iter().map(|&(x, y)| g.p(x, y)).collect();
    let shape = if closed {
        PathShape::closed_line(points, stroke)
    } else {
        PathShape::line(points, stroke)
    };
    painter.add(Shape::Path(shape));
}

fn arc(g: &G, cx: f32, cy: f32, r: f32, a0: f32, a1: f32) -> Vec<Pos2> {
    let n = 24;
    (0..=n)
        .map(|i| {
            let a = (a0 + (a1 - a0) * i as f32 / n as f32).to_radians();
            g.p(cx + r * a.cos(), cy + r * a.sin())
        })
        .collect()
}

pub fn paint_icon(painter: &egui::Painter, center: Pos2, size: f32, icon: Icon, color: Color32) {
    let g = G { c: center, k: size / 24.0 };
    let w = ICON_STROKE * (size / ICON_SIZE).max(1.0);
    let s = Stroke::new(w, color);
    let line = |a: (f32, f32), b: (f32, f32)| {
        painter.line_segment([g.p(a.0, a.1), g.p(b.0, b.1)], s);
    };
    match icon {
        Icon::ChevronLeft => poly(painter, &g, &[(15.0, 18.0), (9.0, 12.0), (15.0, 6.0)], false, s),
        Icon::ChevronRight => poly(painter, &g, &[(9.0, 18.0), (15.0, 12.0), (9.0, 6.0)], false, s),
        Icon::ChevronUp => poly(painter, &g, &[(6.0, 15.0), (12.0, 9.0), (18.0, 15.0)], false, s),
        Icon::ChevronDown => poly(painter, &g, &[(6.0, 9.0), (12.0, 15.0), (18.0, 9.0)], false, s),
        Icon::Play => {
            // Оптический сдвиг +1 вправо: центр масс треугольника.
            let pts = vec![g.p(8.0, 5.0), g.p(20.0, 12.0), g.p(8.0, 19.0)];
            painter.add(Shape::convex_polygon(pts.clone(), color, Stroke::new(w, color)));
        }
        Icon::Pause => {
            for x in [6.5, 14.5] {
                let r = Rect::from_min_max(g.p(x, 5.0), g.p(x + 3.5, 19.0));
                painter.rect_filled(r, 1, color);
            }
        }
        Icon::ZoomIn | Icon::ZoomOut => {
            painter.circle_stroke(g.p(11.0, 11.0), 7.0 * g.k, s);
            line((20.5, 20.5), (16.0, 16.0));
            line((8.0, 11.0), (14.0, 11.0));
            if icon == Icon::ZoomIn {
                line((11.0, 8.0), (11.0, 14.0));
            }
        }
        Icon::Rotate | Icon::Replay => {
            let cw = icon == Icon::Rotate;
            let pts = if cw { arc(&g, 12.0, 12.0, 8.5, -60.0, 230.0) } else { arc(&g, 12.0, 12.0, 8.5, 240.0, -50.0) };
            painter.add(Shape::Path(PathShape::line(pts, s)));
            if cw {
                poly(painter, &g, &[(21.0, 3.5), (20.5, 8.5), (15.5, 8.0)], false, s);
            } else {
                poly(painter, &g, &[(3.0, 3.5), (3.5, 8.5), (8.5, 8.0)], false, s);
            }
        }
        Icon::Info => {
            painter.circle_stroke(g.p(12.0, 12.0), 10.0 * g.k, s);
            line((12.0, 16.5), (12.0, 11.0));
            painter.circle_filled(g.p(12.0, 7.75), w * 0.9, color);
        }
        Icon::Trash => {
            line((3.5, 6.0), (20.5, 6.0));
            poly(painter, &g, &[(18.5, 6.0), (17.6, 19.2), (16.0, 21.0), (8.0, 21.0), (6.4, 19.2), (5.5, 6.0)], false, s);
            poly(painter, &g, &[(9.0, 6.0), (9.0, 3.5), (15.0, 3.5), (15.0, 6.0)], false, s);
            line((10.0, 10.5), (10.0, 16.5));
            line((14.0, 10.5), (14.0, 16.5));
        }
        Icon::Fullscreen => {
            for pts in [
                [(8.0, 3.0), (3.0, 3.0), (3.0, 8.0)],
                [(16.0, 3.0), (21.0, 3.0), (21.0, 8.0)],
                [(21.0, 16.0), (21.0, 21.0), (16.0, 21.0)],
                [(3.0, 16.0), (3.0, 21.0), (8.0, 21.0)],
            ] {
                poly(painter, &g, &pts, false, s);
            }
        }
        Icon::ExitFullscreen => {
            for pts in [
                [(8.0, 3.0), (8.0, 8.0), (3.0, 8.0)],
                [(16.0, 3.0), (16.0, 8.0), (21.0, 8.0)],
                [(21.0, 16.0), (16.0, 16.0), (16.0, 21.0)],
                [(3.0, 16.0), (8.0, 16.0), (8.0, 21.0)],
            ] {
                poly(painter, &g, &pts, false, s);
            }
        }
        Icon::Volume | Icon::VolumeLow | Icon::Mute => {
            poly(
                painter,
                &g,
                &[(11.0, 5.0), (6.0, 9.0), (2.5, 9.0), (2.5, 15.0), (6.0, 15.0), (11.0, 19.0)],
                true,
                s,
            );
            match icon {
                Icon::Mute => {
                    line((22.0, 9.0), (16.0, 15.0));
                    line((16.0, 9.0), (22.0, 15.0));
                }
                _ => {
                    painter.add(Shape::Path(PathShape::line(arc(&g, 11.0, 12.0, 4.5, -45.0, 45.0), s)));
                    if icon == Icon::Volume {
                        painter.add(Shape::Path(PathShape::line(arc(&g, 11.0, 12.0, 8.5, -50.0, 50.0), s)));
                    }
                }
            }
        }
        Icon::Loop => {
            poly(painter, &g, &[(17.0, 2.0), (21.0, 6.0), (17.0, 10.0)], false, s);
            poly(painter, &g, &[(3.0, 12.0), (3.0, 10.0), (6.0, 6.0), (21.0, 6.0)], false, s);
            poly(painter, &g, &[(7.0, 22.0), (3.0, 18.0), (7.0, 14.0)], false, s);
            poly(painter, &g, &[(21.0, 12.0), (21.0, 14.0), (18.0, 18.0), (3.0, 18.0)], false, s);
        }
        Icon::Folder => {
            poly(
                painter,
                &g,
                &[(3.0, 6.0), (3.0, 19.0), (21.0, 19.0), (21.0, 8.5), (12.0, 8.5), (10.0, 5.0), (3.0, 5.0)],
                true,
                s,
            );
        }
        Icon::Copy => {
            let r = Rect::from_min_max(g.p(8.5, 8.5), g.p(21.0, 21.0));
            painter.rect_stroke(r, (2.0 * g.k) as u8, s, egui::StrokeKind::Middle);
            poly(painter, &g, &[(15.5, 5.0), (15.5, 3.0), (3.0, 3.0), (3.0, 15.5), (5.0, 15.5)], false, s);
        }
        Icon::StepBack => {
            line((6.0, 5.0), (6.0, 19.0));
            poly(painter, &g, &[(19.0, 5.0), (9.5, 12.0), (19.0, 19.0)], true, s);
        }
        Icon::StepFwd => {
            line((18.0, 5.0), (18.0, 19.0));
            poly(painter, &g, &[(5.0, 5.0), (14.5, 12.0), (5.0, 19.0)], true, s);
        }
        Icon::Close => {
            line((18.0, 6.0), (6.0, 18.0));
            line((6.0, 6.0), (18.0, 18.0));
        }
        Icon::OpenWith => {
            poly(painter, &g, &[(14.0, 3.0), (21.0, 3.0), (21.0, 10.0)], false, s);
            line((10.0, 14.0), (21.0, 3.0));
            poly(painter, &g, &[(18.0, 13.5), (18.0, 19.0), (16.0, 21.0), (5.0, 21.0), (3.0, 19.0), (3.0, 8.0), (5.0, 6.0), (10.5, 6.0)], false, s);
        }
    }
}

// ---------------------------------------------------------------------------
// Пружина нажатия: сжатие ≤ 90 мс, возврат с перелётом ≈1%.

#[derive(Clone, Copy)]
struct Press {
    v: f32,
    vel: f32,
    held_until: f64,
}

/// Масштаб контрола для эффекта нажатия.
pub fn press_scale(ui: &Ui, id: Id, resp: &Response, size: Vec2) -> f32 {
    let ctx = ui.ctx();
    let now = ctx.input(|i| i.time);
    let dt = ctx.input(|i| i.stable_dt).clamp(0.001, 0.05);
    let mut st = ctx.data_mut(|d| *d.get_temp_mut_or(id, Press { v: 1.0, vel: 0.0, held_until: 0.0 }));
    if resp.is_pointer_button_down_on() || (resp.has_focus() && ctx.input(|i| i.key_down(egui::Key::Enter))) {
        st.held_until = now + crate::theme::M_PRESS_MIN;
    }
    let pressed = now < st.held_until;
    let reduced = crate::app::reduced_motion();
    let target = if pressed { 1.0 - 8.0 / (size.x + size.y) } else { 1.0 };
    if reduced {
        st.v = 1.0;
        st.vel = 0.0;
    } else if pressed {
        st.v += (target - st.v) * (1.0 - (-dt / 0.025).exp());
        st.vel = 0.0;
    } else {
        let f = -520.0 * (st.v - target) - 26.0 * st.vel;
        st.vel += f * dt;
        st.v += st.vel * dt;
    }
    let settled = (st.v - target).abs() < 0.0005 && st.vel.abs() < 0.001;
    if settled {
        st.v = target;
        st.vel = 0.0;
    }
    if !settled || pressed {
        ctx.request_repaint();
    }
    ctx.data_mut(|d| d.insert_temp(id, st));
    st.v
}

fn pressed_now(ui: &Ui, id: Id) -> bool {
    let now = ui.ctx().input(|i| i.time);
    ui.ctx()
        .data(|d| d.get_temp::<Press>(id))
        .map(|p| now < p.held_until)
        .unwrap_or(false)
}

/// Круглая иконка-кнопка.
pub fn icon_button(ui: &mut Ui, id: Id, rect: Rect, icon: Icon, tip: &str, enabled: bool, alpha: f32) -> Response {
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let resp = ui.interact(rect, id, sense);
    let scale = press_scale(ui, id.with("press"), &resp, rect.size());
    let hover = ui.ctx().animate_bool_with_time(id.with("hover"), enabled && resp.hovered(), M_MICRO);
    let pressed = enabled && pressed_now(ui, id.with("press"));
    let painter = ui.painter();
    let r = rect.size().x * 0.5 * scale;
    if pressed {
        painter.circle_filled(rect.center(), r, SURFACE_PRESSED.gamma_multiply(alpha));
    } else if hover > 0.0 {
        painter.circle_filled(rect.center(), r, SURFACE_RAISED.gamma_multiply(hover * alpha));
    }
    let color = if enabled { TEXT } else { TEXT_SECONDARY.gamma_multiply(0.45) };
    paint_icon(painter, rect.center(), ICON_SIZE * scale, icon, color.gamma_multiply(alpha));
    if resp.has_focus() {
        painter.circle_stroke(rect.center(), rect.width() * 0.5 + 2.0, Stroke::new(2.0, ACCENT));
    }
    if enabled && resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    if !tip.is_empty() && alpha > 0.5 {
        resp.on_hover_text(tip)
    } else {
        resp
    }
}

/// Пилюля с текстом. `primary` — заливка action, иначе ghost с контуром.
pub fn pill_button(ui: &mut Ui, id: Id, center: Pos2, label: &str, primary: bool) -> Response {
    let font = regular(T_SUBHEAD + 1.0);
    let galley = ui.painter().layout_no_wrap(label.to_owned(), font, TEXT);
    let size = vec2(galley.size().x + 44.0, PILL_H);
    let rect = Rect::from_center_size(center, size);
    let resp = ui.interact(rect, id, Sense::click());
    let scale = press_scale(ui, id.with("press"), &resp, size);
    let hover = ui.ctx().animate_bool_with_time(id.with("hover"), resp.hovered(), M_MICRO);
    let pressed = pressed_now(ui, id.with("press"));
    let r = Rect::from_center_size(center, size * scale);
    let painter = ui.painter();
    let rad = (r.height() / 2.0) as u8;
    let text_color;
    if primary {
        let base = if pressed { Color32::from_rgb(0xd2, 0xd2, 0xd7) } else { ACTION };
        let fill = lerp_color(base, Color32::WHITE, hover * 0.6);
        painter.rect_filled(r, rad, fill);
        text_color = ON_ACTION;
    } else {
        let fill = if pressed { SURFACE_PRESSED } else { SURFACE_RAISED.gamma_multiply(hover) };
        painter.rect_filled(r, rad, fill);
        painter.rect_stroke(r, rad, Stroke::new(1.0, FIELD_BORDER), egui::StrokeKind::Inside);
        text_color = TEXT;
    }
    let galley = ui.painter().layout_no_wrap(label.to_owned(), regular((T_SUBHEAD + 1.0) * scale), text_color);
    let pos = r.center() - galley.size() * 0.5;
    ui.painter().galley(pos, galley, text_color);
    if resp.has_focus() {
        ui.painter().rect_stroke(r.expand(3.0), rad + 3, Stroke::new(2.0, ACCENT), egui::StrokeKind::Outside);
    }
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

/// Текстовая ссылка с шевроном «›».
pub fn link(ui: &mut Ui, id: Id, pos: Pos2, anchor: Align2, label: &str) -> Response {
    let text = format!("{label} ›");
    let galley = ui.painter().layout_no_wrap(text, regular(T_SUBHEAD), ACCENT);
    let rect = anchor.anchor_size(pos, galley.size()).expand2(vec2(4.0, 6.0));
    let resp = ui.interact(rect, id, Sense::click());
    let hover = ui.ctx().animate_bool_with_time(id.with("hover"), resp.hovered(), M_MICRO);
    let color = lerp_color(ACCENT, Color32::from_rgb(0x6c, 0xb8, 0xff), hover);
    ui.painter().galley(rect.min + vec2(4.0, 6.0), galley, color);
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        ui.painter().line_segment(
            [pos2(rect.min.x + 4.0, rect.max.y - 5.0), pos2(rect.max.x - 4.0, rect.max.y - 5.0)],
            Stroke::new(1.0, color.gamma_multiply(hover)),
        );
    }
    if resp.has_focus() {
        ui.painter().rect_stroke(rect, 6, Stroke::new(2.0, ACCENT), egui::StrokeKind::Outside);
    }
    resp
}

pub fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgba_premultiplied(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()), l(a.a(), b.a()))
}

/// Горизонтальный ползунок 0..1. Возвращает (response, новое значение при изменении).
pub fn slider(ui: &mut Ui, id: Id, rect: Rect, value: f32, alpha: f32) -> (Response, Option<f32>) {
    let resp = ui.interact(rect, id, Sense::click_and_drag());
    let mut new = None;
    if resp.dragged() || resp.clicked() || resp.drag_started() {
        if let Some(p) = resp.interact_pointer_pos() {
            new = Some(((p.x - rect.left()) / rect.width()).clamp(0.0, 1.0));
        }
    }
    let v = new.unwrap_or(value);
    let hover = ui.ctx().animate_bool_with_time(id.with("h"), resp.hovered() || resp.dragged(), M_MICRO);
    let th = 4.0 + 2.0 * hover;
    let track = Rect::from_center_size(rect.center(), vec2(rect.width(), th));
    let painter = ui.painter();
    painter.rect_filled(track, (th / 2.0) as u8, Color32::from_white_alpha((38.0 * alpha) as u8));
    let mut fill = track;
    fill.max.x = track.left() + track.width() * v;
    painter.rect_filled(fill, (th / 2.0) as u8, TEXT.gamma_multiply(alpha));
    let knob = 5.0 + 2.0 * hover;
    painter.circle_filled(pos2(fill.max.x, track.center().y), knob, TEXT.gamma_multiply(alpha));
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    (resp, new)
}

/// Спиннер ожидания (тонкое кольцо).
pub fn spinner(painter: &egui::Painter, center: Pos2, r: f32, t: f64, alpha: f32) {
    let start = (t * 300.0) as f32 % 360.0;
    let g = G { c: center, k: 1.0 };
    let pts: Vec<Pos2> = (0..=28)
        .map(|i| {
            let a = (start + 270.0 * i as f32 / 28.0).to_radians();
            g.p(12.0 + r * a.cos(), 12.0 + r * a.sin())
        })
        .collect();
    painter.circle_stroke(center, r, Stroke::new(2.0, Color32::from_white_alpha((25.0 * alpha) as u8)));
    painter.add(Shape::Path(PathShape::line(pts, Stroke::new(2.0, TEXT.gamma_multiply(alpha)))));
}

/// Уголки видоискателя — фирменная деталь.
pub fn viewfinder(painter: &egui::Painter, rect: Rect, len: f32, stroke: Stroke) {
    let (l, t, r, b) = (rect.left(), rect.top(), rect.right(), rect.bottom());
    for pts in [
        [pos2(l, t + len), pos2(l, t), pos2(l + len, t)],
        [pos2(r - len, t), pos2(r, t), pos2(r, t + len)],
        [pos2(r, b - len), pos2(r, b), pos2(r - len, b)],
        [pos2(l + len, b), pos2(l, b), pos2(l, b - len)],
    ] {
        painter.add(Shape::Path(PathShape::line(pts.to_vec(), stroke)));
    }
}

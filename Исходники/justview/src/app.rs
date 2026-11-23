//! Главное окно JustView.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use egui::epaint::{Mesh, Vertex};
use egui::{
    Align2, Color32, ColorImage, CursorIcon, Id, ImageData, Key, Pos2, Rect, RichText, Sense, Shape,
    Stroke, TextureFilter, TextureHandle, TextureOptions, TextureWrapMode, Ui, UiBuilder, Vec2,
    ViewportCommand, pos2, vec2,
};

use crate::audio::AudioOut;
use crate::decode::{Content, Decoded};
use crate::formats::{Kind, ext_of, kind_label, kind_of};
use crate::loader::{Done, Job, Loader};
use crate::player::{Player, SPEEDS};
use crate::settings::Settings;
use crate::theme::*;
use crate::widgets::{self, Icon, icon_button, link, pill_button};
use crate::{register, winutil};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

static REDUCED: AtomicBool = AtomicBool::new(false);
pub fn reduced_motion() -> bool {
    REDUCED.load(Ordering::Relaxed)
}

const TEX_STATIC: TextureOptions = TextureOptions {
    magnification: TextureFilter::Linear,
    minification: TextureFilter::Linear,
    wrap_mode: TextureWrapMode::ClampToEdge,
    mipmap_mode: Some(TextureFilter::Linear),
};
const TEX_DYNAMIC: TextureOptions = TextureOptions::LINEAR;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Act {
    Prev,
    Next,
    First,
    Last,
    ZoomIn,
    ZoomOut,
    ZoomAt(f32, Option<Pos2>),
    ToggleFit(Option<Pos2>),
    Actual,
    RotateCw,
    RotateCcw,
    Info,
    AskDelete,
    Fullscreen,
    ExitFullscreenOrClose,
    Copy,
    CopyPath,
    PlayPause,
    StepBack,
    StepFwd,
    Seek(f64),
    SeekRel(f64),
    Mute,
    Volume(f32),
    VolumeRel(f32),
    Loop,
    SpeedMenu,
    SetSpeed(f32),
    SpeedStep(i32),
    TrackMenu,
    SetTrack(usize),
    TrackCycle,
    PagePrev,
    PageNext,
    OpenWith,
    ShowFolder,
    Open,
    MakeDefault,
    Help,
}

#[derive(Clone, Copy)]
struct View {
    fit: bool,
    scale: f32,
    offset: Vec2,
    rot: u8,
    d_scale: f32,
    d_offset: Vec2,
    ready: bool,
}

impl Default for View {
    fn default() -> Self {
        Self { fit: true, scale: 1.0, offset: Vec2::ZERO, rot: 0, d_scale: 1.0, d_offset: Vec2::ZERO, ready: false }
    }
}

struct Anim {
    frame: usize,
    acc: f64,
    paused: bool,
}

enum Dialog {
    None,
    Delete(PathBuf),
    Help,
    /// Language picker; `true` on first run (cannot be dismissed without a choice).
    Language(bool),
}

const REPO: &str = "Spamerovith/JustView";
const UA: &str = "JustView-update-check";

struct Toast {
    text: String,
    at: f64,
}

struct Drawn {
    tex: TextureHandle,
    rect: Rect,
    rot: u8,
}

pub struct App {
    loader: Loader,
    settings: Settings,
    files: Vec<PathBuf>,
    index: usize,
    cache: HashMap<PathBuf, Arc<Decoded>>,
    tex: HashMap<PathBuf, Vec<Option<TextureHandle>>>,
    view: View,
    anim: Anim,
    player: Option<Player>,
    player_path: Option<PathBuf>,
    video_tex: Option<TextureHandle>,
    audio_out: Option<AudioOut>,
    audio_tried: bool,
    speed: f32,
    pdf_page: u32,
    pdf_tex: Option<(PathBuf, u32, TextureHandle, [u32; 2])>,
    pdf_requested: Option<(PathBuf, u32)>,
    info_open: bool,
    dialog: Dialog,
    toast: Option<Toast>,
    last_activity: f64,
    speed_menu: bool,
    track_menu: bool,
    fade_from: Option<Drawn>,
    last_drawn: Option<Drawn>,
    shown_at: Option<(PathBuf, f64)>,
    opened_at: f64,
    titled: Option<PathBuf>,
    styled: bool,
    scrubbing: Option<bool>,
    text_lines: Option<(PathBuf, Vec<String>)>,
    chrome_rects: Vec<Rect>,
    pending_open: Option<PathBuf>,
    update: Option<Arc<std::sync::Mutex<Option<crate::update::Release>>>>,
    update_hidden: bool,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, loader: Loader, initial: Option<PathBuf>, settings: Settings) -> Self {
        install_fonts(&cc.egui_ctx);
        install_style(&cc.egui_ctx);
        loader.set_ctx(&cc.egui_ctx);
        REDUCED.store(reduced_motion_sys(), Ordering::Relaxed);
        let mut app = Self {
            loader,
            settings,
            files: Vec::new(),
            index: 0,
            cache: HashMap::new(),
            tex: HashMap::new(),
            view: View::default(),
            anim: Anim { frame: 0, acc: 0.0, paused: false },
            player: None,
            player_path: None,
            video_tex: None,
            audio_out: None,
            audio_tried: false,
            speed: 1.0,
            pdf_page: 0,
            pdf_tex: None,
            pdf_requested: None,
            info_open: false,
            dialog: Dialog::None,
            toast: None,
            last_activity: 0.0,
            speed_menu: false,
            track_menu: false,
            fade_from: None,
            last_drawn: None,
            shown_at: None,
            opened_at: 0.0,
            titled: None,
            styled: false,
            scrubbing: None,
            text_lines: None,
            chrome_rects: Vec::new(),
            pending_open: None,
            update: None,
            update_hidden: false,
        };
        if app.settings.lang.is_none() {
            app.dialog = Dialog::Language(true);
        }
        // Не чаще раза в сутки: JustView открывается на каждый файл.
        let now = crate::update::now();
        if now.saturating_sub(app.settings.update_checked) > 24 * 3600 {
            app.settings.update_checked = now;
            app.settings.save();
            let ctx = cc.egui_ctx.clone();
            app.update = Some(crate::update::spawn_check(REPO, UA, VERSION, app.settings.skip_version.clone(), move || ctx.request_repaint()));
        }
        if let Some(p) = initial {
            // Декодирование уже запущено в main() до создания окна.
            app.files = vec![p];
            app.index = 0;
        }
        app
    }

    pub fn open_later(&mut self, p: PathBuf) {
        self.pending_open = Some(p);
    }

    fn cur(&self) -> Option<&PathBuf> {
        self.files.get(self.index)
    }

    /// Открыть файл (из Проводника, диалога, перетаскивания).
    fn open(&mut self, path: PathBuf) {
        let path = if path.is_dir() {
            let list = crate::loader::scan_dir(&path, &path.join("_"));
            match list.into_iter().find(|p| kind_of(p) != Kind::Other) {
                Some(p) => p,
                None => {
                    self.toast(tr!("В этой папке нет файлов для просмотра", "No viewable files in this folder", "У цій теці немає файлів для перегляду"));
                    return;
                }
            }
        } else {
            path
        };
        self.loader.request(Job::Decode(path.clone()), true);
        if let Some(dir) = path.parent() {
            self.loader.request(Job::ScanDir(dir.to_path_buf(), path.clone()), false);
        }
        self.files = vec![path];
        self.set_index(0);
    }

    fn open_many(&mut self, mut paths: Vec<PathBuf>) {
        paths.retain(|p| p.is_file());
        if paths.len() <= 1 {
            if let Some(p) = paths.pop() {
                self.open(p);
            }
            return;
        }
        paths.sort_by(|a, b| winutil::natural_cmp(a.as_os_str(), b.as_os_str()));
        self.files = paths;
        self.set_index(0);
    }

    fn set_index(&mut self, i: usize) {
        if self.files.is_empty() {
            return;
        }
        let i = i.min(self.files.len() - 1);
        if let Some(d) = self.last_drawn.take() {
            self.fade_from = Some(d);
        }
        self.index = i;
        self.view = View::default();
        self.anim = Anim { frame: 0, acc: 0.0, paused: false };
        self.player = None;
        self.player_path = None;
        self.video_tex = None;
        self.pdf_page = 0;
        self.pdf_requested = None;
        self.speed_menu = false;
        self.track_menu = false;
        self.shown_at = None;
        self.scrubbing = None;
        self.opened_at = f64::NAN;
        let p = self.files[i].clone();
        self.loader.request(Job::Decode(p.clone()), true);
        self.preload();
    }

    fn neighbors(&self) -> Vec<PathBuf> {
        let n = self.files.len();
        let mut v = Vec::new();
        if n == 0 {
            return v;
        }
        v.push(self.files[self.index].clone());
        for d in [1isize, -1, 2] {
            if (d.unsigned_abs()) < n {
                let j = (self.index as isize + d).rem_euclid(n as isize) as usize;
                v.push(self.files[j].clone());
            }
        }
        v
    }

    fn preload(&mut self) {
        let wanted = self.neighbors();
        self.loader.retain_decodes(&wanted);
        for p in wanted.iter().skip(1) {
            if !self.cache.contains_key(p) {
                self.loader.request(Job::Decode(p.clone()), false);
            }
        }
        let keep: Vec<PathBuf> = wanted;
        self.cache.retain(|k, _| keep.contains(k));
        self.tex.retain(|k, _| keep.contains(k));
    }

    fn go(&mut self, d: isize) {
        let n = self.files.len();
        if n <= 1 {
            return;
        }
        let j = (self.index as isize + d).rem_euclid(n as isize) as usize;
        self.set_index(j);
    }

    fn toast(&mut self, text: &str) {
        self.toast = Some(Toast { text: text.to_string(), at: f64::NAN });
    }

    fn poll(&mut self, ctx: &egui::Context) {
        for done in self.loader.poll() {
            match done {
                Done::Decoded(p, d) => {
                    if self.neighbors().contains(&p) {
                        self.cache.insert(p, d);
                    }
                }
                Done::Dir(dir, list) => {
                    let Some(cur) = self.cur().cloned() else { continue };
                    if cur.parent() != Some(dir.as_path()) || self.files.len() > 1 {
                        continue;
                    }
                    let mut list = list;
                    let idx = match list.iter().position(|p| *p == cur) {
                        Some(i) => i,
                        None => {
                            list.insert(0, cur.clone());
                            0
                        }
                    };
                    self.files = list;
                    self.index = idx;
                    self.preload();
                }
                Done::PdfPage(p, page, r) => {
                    if Some(&p) != self.cur() {
                        continue;
                    }
                    match r {
                        Ok(img) => {
                            let size = [img.size[0] as u32, img.size[1] as u32];
                            let t = ctx.load_texture("pdf", ImageData::Color(img), TEX_STATIC);
                            if page == self.pdf_page {
                                self.pdf_tex = Some((p, page, t, size));
                            }
                        }
                        Err(e) => self.toast(&trf!(["Страница не отрисована: {e}", "Page could not be rendered: {e}", "Сторінку не вдалося відобразити: {e}"])),
                    }
                }
            }
        }
    }

    fn ensure_audio(&mut self) -> Option<Arc<crate::audio::Shared>> {
        if !self.audio_tried {
            self.audio_tried = true;
            self.audio_out = AudioOut::new();
        }
        self.audio_out.as_ref().map(|a| a.shared.clone())
    }

    fn apply_volume(&self) {
        if let Some(a) = &self.audio_out {
            let v = if self.settings.muted { 0.0 } else { self.settings.volume };
            a.shared.set_volume(v);
        }
    }

    // ------------------------------------------------------------------
    // Геометрия просмотра

    fn content_size(&self, d: &Decoded) -> Option<[f32; 2]> {
        let s = match &d.content {
            Content::Image { orig, .. } => [orig[0] as f32, orig[1] as f32],
            Content::Media { info, .. } if info.has_video => [info.width as f32, info.height as f32],
            Content::Media { cover: Some(c), .. } => [c.size[0] as f32, c.size[1] as f32],
            Content::Pdf { first, .. } => match &self.pdf_tex {
                Some((_, _, _, s)) => [s[0] as f32, s[1] as f32],
                None => [first.size[0] as f32, first.size[1] as f32],
            },
            _ => return None,
        };
        if s[0] <= 0.0 || s[1] <= 0.0 {
            return None;
        }
        Some(if self.view.rot % 2 == 1 { [s[1], s[0]] } else { s })
    }

    fn fit_scale(&self, size: [f32; 2], area: Rect, ppp: f32, is_pdf: bool) -> f32 {
        let s = (area.width() / size[0]).min(area.height() / size[1]);
        if is_pdf { s } else { s.min(1.0 / ppp) }
    }

    fn zoom_to(&mut self, new_scale: f32, at: Option<Pos2>, area: Rect, min_s: f32, ppp: f32) {
        let max_s = 64.0 / ppp;
        let new_scale = new_scale.clamp(min_s.min(0.02), max_s);
        let anchor = at.unwrap_or(area.center());
        let c = area.center() + self.view.offset;
        let u = (anchor - c) / self.view.scale;
        let c2 = anchor - u * new_scale;
        self.view.offset = c2 - area.center();
        self.view.scale = new_scale;
        self.view.fit = false;
    }

    fn clamp_offset(&mut self, size: [f32; 2], area: Rect) {
        let iw = size[0] * self.view.scale;
        let ih = size[1] * self.view.scale;
        let mx = ((iw - area.width()) / 2.0).max(0.0);
        let my = ((ih - area.height()) / 2.0).max(0.0);
        self.view.offset.x = self.view.offset.x.clamp(-mx, mx);
        self.view.offset.y = self.view.offset.y.clamp(-my, my);
    }
}

fn reduced_motion_sys() -> bool {
    winutil::animations_disabled()
}

fn draw_tex(painter: &egui::Painter, tex: egui::TextureId, rect: Rect, rot: u8, tint: Color32) {
    let mut mesh = Mesh::with_texture(tex);
    let uvs = [pos2(0.0, 0.0), pos2(1.0, 0.0), pos2(1.0, 1.0), pos2(0.0, 1.0)];
    let corners = [rect.left_top(), rect.right_top(), rect.right_bottom(), rect.left_bottom()];
    let k = (rot % 4) as usize;
    for i in 0..4 {
        mesh.vertices.push(Vertex { pos: corners[i], uv: uvs[(i + 4 - k) % 4], color: tint });
    }
    mesh.indices = vec![0, 1, 2, 0, 2, 3];
    painter.add(Shape::mesh(mesh));
}

fn fmt_time(t: f64) -> String {
    let t = t.max(0.0);
    let s = t.floor() as u64;
    let (h, m, s) = (s / 3600, (s / 60) % 60, s % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}

fn fmt_bytes(b: u64) -> String {
    let f = b as f64;
    let s = if f >= 1024.0 * 1024.0 * 1024.0 {
        trf!(["{:.2} ГБ", "{:.2} GB", "{:.2} ГБ"], f / 1024f64.powi(3))
    } else if f >= 1024.0 * 1024.0 {
        trf!(["{:.1} МБ", "{:.1} MB", "{:.1} МБ"], f / 1024f64.powi(2))
    } else if f >= 1024.0 {
        trf!(["{:.0} КБ", "{:.0} KB", "{:.0} КБ"], f / 1024.0)
    } else {
        trf!(["{b} Б", "{b} B", "{b} Б"])
    };
    s.replace('.', ",")
}

fn fmt_speed(s: f32) -> String {
    let t = format!("{s}");
    format!("{}×", t.replace('.', ","))
}

/// Короткое имя дорожки для кнопки: без пояснения в скобках.
fn short_track(label: &str) -> String {
    match label.split_once(" (") {
        Some((a, _)) => a.to_string(),
        None => label.to_string(),
    }
}

/// Обрезает строку с многоточием под ширину `max_w` при размере шрифта `size`.
fn ellipsize(painter: &egui::Painter, s: &str, size: f32, max_w: f32) -> String {
    let fits = |t: &str| painter.layout_no_wrap(t.to_string(), regular(size), Color32::WHITE).size().x <= max_w;
    if fits(s) {
        return s.to_string();
    }
    let chars: Vec<char> = s.chars().collect();
    let mut n = chars.len();
    while n > 1 {
        n -= 1;
        let t: String = chars[..n].iter().collect::<String>() + "…";
        if fits(&t) {
            return t;
        }
    }
    "…".into()
}

fn ease(t: f32) -> f32 {
    // cubic-bezier(0.28, 0.11, 0.32, 1) ≈ easeOutCubic
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if !self.styled {
            self.styled = true;
            use raw_window_handle::{HasWindowHandle, RawWindowHandle};
            if let Ok(h) = frame.window_handle() {
                if let RawWindowHandle::Win32(w) = h.as_raw() {
                    winutil::style_titlebar(w.hwnd.get());
                }
            }
            let mts = ctx.input(|i| i.max_texture_side) as u32;
            if mts >= 4096 {
                self.loader.max_side.store(mts.min(16384), Ordering::Relaxed);
            }
        }
        let now = ctx.input(|i| i.time);
        if self.opened_at.is_nan() {
            self.opened_at = now;
        }
        self.poll(&ctx);
        let (maxed, fs_now, inner) = ctx.input(|i| {
            let v = i.viewport();
            (v.maximized.unwrap_or(false), v.fullscreen.unwrap_or(false), v.inner_rect)
        });
        if !fs_now {
            self.settings.maximized = maxed;
            if !maxed {
                if let Some(r) = inner {
                    self.settings.width = r.width();
                    self.settings.height = r.height();
                }
            }
        }
        if let Some(p) = self.pending_open.take() {
            self.open(p);
        }

        // Перетаскивание файлов в окно.
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect());
        if !dropped.is_empty() {
            self.open_many(dropped);
        }
        let hovering_files = ctx.input(|i| !i.raw.hovered_files.is_empty());

        // Активность пользователя → показать панели.
        let active = ctx.input(|i| {
            i.pointer.is_moving() || i.pointer.any_down() || !i.events.is_empty() && i.events.iter().any(|e| matches!(e, egui::Event::Key { .. } | egui::Event::MouseWheel { .. }))
        });
        if active {
            self.last_activity = now;
        }

        let full = ui.max_rect();
        let ppp = ctx.pixels_per_point();
        let info_t = ease(ctx.animate_bool_with_time(Id::new("info_anim"), self.info_open, if reduced_motion() { 0.0 } else { M_MEDIUM }));
        let view_rect = Rect::from_min_max(full.min, pos2(full.max.x - INFO_W * info_t, full.max.y));

        let cur = self.cur().cloned();
        let decoded = cur.as_ref().and_then(|p| self.cache.get(p).cloned());

        // Заголовок окна
        if self.titled != cur {
            self.titled = cur.clone();
            let t = match &cur {
                Some(p) => format!("{} - JustView", p.file_name().unwrap_or_default().to_string_lossy()),
                None => "JustView".into(),
            };
            ctx.send_viewport_cmd(ViewportCommand::Title(t));
        }

        // Плеер для видео/аудио
        if let (Some(p), Some(d)) = (&cur, &decoded) {
            if let Content::Media { info, .. } = &d.content {
                if self.player_path.as_ref() != Some(p) {
                    let audio = if info.has_audio { self.ensure_audio() } else { None };
                    self.apply_volume();
                    let mbox = winutil::work_area().map(|(w, h)| [w as u32, h as u32]).unwrap_or([2560, 1440]);
                    self.player = Some(Player::new(p, info.clone(), audio, &ctx, mbox, self.speed, self.settings.loop_video));
                    self.player_path = Some(p.clone());
                }
            }
        }
        if let Some(pl) = &mut self.player {
            if let Some(after) = pl.update() {
                ctx.request_repaint_after_secs(after as f32);
            }
            if pl.frame_changed {
                if let Some(f) = &pl.current {
                    match &mut self.video_tex {
                        Some(t) if t.size() == f.image.size => t.set(ImageData::Color(f.image.clone()), TEX_DYNAMIC),
                        _ => self.video_tex = Some(ctx.load_texture("video", ImageData::Color(f.image.clone()), TEX_DYNAMIC)),
                    }
                }
            }
        }

        // PDF: запрос текущей страницы в нужном разрешении.
        if let (Some(p), Some(d)) = (&cur, &decoded) {
            if let Content::Pdf { .. } = &d.content {
                let want = (p.clone(), self.pdf_page);
                let have = self.pdf_tex.as_ref().map(|(pp, pg, _, _)| (pp.clone(), *pg));
                if have.as_ref() != Some(&want) && self.pdf_requested.as_ref() != Some(&want) {
                    let width = ((view_rect.width() * ppp * 1.5) as u32).clamp(1200, 3200);
                    self.loader.request(Job::PdfPage(p.clone(), self.pdf_page, width), true);
                    self.pdf_requested = Some(want);
                }
            }
        }

        let mut acts: Vec<Act> = Vec::new();
        self.keyboard(&ctx, decoded.as_deref(), &mut acts);

        // ---- Холст ----
        let painter = ui.painter().clone();
        painter.rect_filled(full, 0, CANVAS);
        let canvas = ui.interact(view_rect, Id::new("canvas"), Sense::click_and_drag());

        let kind = cur.as_ref().map(|p| kind_of(p)).unwrap_or(Kind::Other);
        let mut has_visual = false;
        let mut chrome_force = false;

        if let (Some(path), Some(d)) = (&cur, &decoded) {
            if self.shown_at.as_ref().map(|(p, _)| p != path).unwrap_or(true) {
                self.shown_at = Some((path.clone(), now));
            }
            let fade = if reduced_motion() { 1.0 } else { (((now - self.shown_at.as_ref().unwrap().1) as f32) / M_FADE).clamp(0.0, 1.0) };

            // Текстура для текущего кадра
            let mut tex_id: Option<TextureHandle> = None;
            match &d.content {
                Content::Image { frames, .. } => {
                    let fi = self.anim.frame.min(frames.len() - 1);
                    let entry = self.tex.entry(path.clone()).or_insert_with(|| vec![None; frames.len()]);
                    if entry[fi].is_none() {
                        let opts = if frames.len() > 1 { TEX_DYNAMIC } else { TEX_STATIC };
                        entry[fi] = Some(ctx.load_texture(format!("img{fi}"), ImageData::Color(frames[fi].image.clone()), opts));
                    }
                    tex_id = entry[fi].clone();
                }
                Content::Media { info, cover } => {
                    if info.has_video {
                        tex_id = self.video_tex.clone();
                    } else if let Some(c) = cover {
                        let entry = self.tex.entry(path.clone()).or_insert_with(|| vec![None]);
                        if entry[0].is_none() {
                            entry[0] = Some(ctx.load_texture("cover", ImageData::Color(c.clone()), TEX_STATIC));
                        }
                        tex_id = entry[0].clone();
                    }
                }
                Content::Pdf { first, .. } => {
                    if let Some((_, _, t, _)) = &self.pdf_tex {
                        tex_id = Some(t.clone());
                    } else if self.pdf_page == 0 {
                        let entry = self.tex.entry(path.clone()).or_insert_with(|| vec![None]);
                        if entry[0].is_none() {
                            entry[0] = Some(ctx.load_texture("pdf0", ImageData::Color(first.clone()), TEX_STATIC));
                        }
                        tex_id = entry[0].clone();
                    }
                }
                _ => {}
            }

            if let (Some(tex), Some(size)) = (tex_id.clone(), self.content_size(d)) {
                has_visual = true;
                let is_cover = matches!(&d.content, Content::Media { info, .. } if !info.has_video);
                let scalable = kind == Kind::Pdf || matches!(ext_of(path).as_str(), "svg" | "svgz");
                let mut fit = self.fit_scale(size, view_rect, ppp, scalable);
                let shift = if is_cover {
                    fit = fit.min(300.0 / size[0].max(size[1]));
                    vec2(0.0, -72.0)
                } else {
                    Vec2::ZERO
                };
                if self.view.fit {
                    self.view.scale = fit;
                    self.view.offset = Vec2::ZERO;
                }
                // Жесты
                let hover = canvas.hovered();
                if hover {
                    let (scroll, zoom, ptr) = ctx.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta(), i.pointer.hover_pos()));
                    let factor = zoom * (scroll / 420.0).exp();
                    if (factor - 1.0).abs() > 1e-4 {
                        acts.push(Act::ZoomAt(factor, ptr));
                    }
                }
                if canvas.dragged() && !self.view.fit {
                    self.view.offset += canvas.drag_delta();
                    self.view.d_offset = self.view.offset;
                    ctx.set_cursor_icon(CursorIcon::Grabbing);
                } else if hover && !self.view.fit {
                    ctx.set_cursor_icon(CursorIcon::Grab);
                }
                if canvas.double_clicked() {
                    if matches!(d.content, Content::Media { .. }) {
                        acts.push(Act::PlayPause); // отменить одиночный клик
                        acts.push(Act::Fullscreen);
                    } else {
                        acts.push(Act::ToggleFit(canvas.interact_pointer_pos()));
                    }
                } else if canvas.clicked() && matches!(d.content, Content::Media { .. }) {
                    acts.push(Act::PlayPause);
                }
                self.clamp_offset(size, view_rect);

                // Плавное приближение к целевому масштабу
                let dt = ctx.input(|i| i.stable_dt).min(0.1);
                if !self.view.ready || reduced_motion() {
                    self.view.d_scale = self.view.scale;
                    self.view.d_offset = self.view.offset;
                    self.view.ready = true;
                } else {
                    let k = 1.0 - (-M_ZOOM_RATE * dt).exp();
                    let ls = self.view.d_scale.ln() + (self.view.scale.ln() - self.view.d_scale.ln()) * k;
                    self.view.d_scale = ls.exp();
                    self.view.d_offset += (self.view.offset - self.view.d_offset) * k;
                    if (self.view.d_scale / self.view.scale - 1.0).abs() > 0.001 || (self.view.d_offset - self.view.offset).length() > 0.3 {
                        ctx.request_repaint();
                    } else {
                        self.view.d_scale = self.view.scale;
                        self.view.d_offset = self.view.offset;
                    }
                }
                let s = self.view.d_scale;
                let img_rect = Rect::from_center_size(view_rect.center() + shift + self.view.d_offset, vec2(size[0] * s, size[1] * s));
                // Пиксель-в-пиксель по физической сетке при 100% и меньше — без размытия границы
                let img_rect = Rect::from_min_size(
                    pos2((img_rect.min.x * ppp).round() / ppp, (img_rect.min.y * ppp).round() / ppp),
                    img_rect.size(),
                );
                let clip = painter.with_clip_rect(view_rect);
                if let Some(old) = &self.fade_from {
                    if fade < 1.0 {
                        draw_tex(&clip, old.tex.id(), old.rect, old.rot, Color32::from_white_alpha(((1.0 - fade) * 255.0) as u8));
                    }
                }
                draw_tex(&clip, tex.id(), img_rect, self.view.rot, Color32::from_white_alpha((fade * 255.0) as u8));
                if fade < 1.0 {
                    ctx.request_repaint();
                } else {
                    self.fade_from = None;
                }
                self.last_drawn = Some(Drawn { tex, rect: img_rect, rot: self.view.rot });
            } else {
                self.fade_from = None;
            }

            // Не-визуальное содержимое
            match &d.content {
                Content::Media { info, cover } if !info.has_video => {
                    chrome_force = true;
                    self.draw_audio(ui, view_rect, info, cover.is_some(), path, &mut acts);
                    if canvas.clicked() {
                        acts.push(Act::PlayPause);
                    }
                }
                Content::Text { text, truncated } => {
                    self.draw_text(ui, view_rect, path, text, *truncated);
                }
                Content::Preview { image } => {
                    chrome_force = true;
                    self.draw_preview(ui, view_rect, path, image.as_ref(), &mut acts);
                }
                Content::Error(e) => {
                    chrome_force = true;
                    self.draw_error(ui, view_rect, path, e, &mut acts);
                }
                Content::Media { info, .. } if info.has_video => {
                    if let Some(pl) = &self.player {
                        if let Some(err) = &pl.error {
                            chrome_force = true;
                            let e = err.clone();
                            self.draw_error(ui, view_rect, path, &e, &mut acts);
                        } else if pl.current.is_none() && pl.ended {
                            chrome_force = true;
                            self.draw_error(ui, view_rect, path, tr!("Видео не удалось декодировать", "Could not decode the video", "Не вдалося декодувати відео"), &mut acts);
                        } else if pl.current.is_none() && now - self.opened_at > 0.15 {
                            widgets::spinner(&painter, view_rect.center(), 14.0, now, 1.0);
                            ctx.request_repaint();
                        }
                    }
                }
                _ => {}
            }
            if let Content::Image { note: Some(n), .. } = &d.content {
                painter.text(
                    pos2(view_rect.center().x, view_rect.top() + SP_24),
                    Align2::CENTER_TOP,
                    n,
                    regular(T_CAPTION),
                    TEXT_SECONDARY,
                );
            }
        } else if cur.is_some() {
            // Загрузка: держим прошлый кадр, после 150 мс — спиннер.
            if let Some(old) = &self.fade_from {
                draw_tex(&painter.with_clip_rect(view_rect), old.tex.id(), old.rect, old.rot, Color32::from_white_alpha(140));
            }
            if now - self.opened_at > 0.15 {
                widgets::spinner(&painter, view_rect.center(), 14.0, now, 1.0);
            }
            ctx.request_repaint();
        } else {
            chrome_force = true;
            self.draw_empty(ui, view_rect, &mut acts);
        }

        // ---- Анимация GIF/WebP/APNG ----
        if let Some(d) = &decoded {
            if let Content::Image { frames, .. } = &d.content {
                if frames.len() > 1 && !self.anim.paused {
                    let dt = ctx.input(|i| i.stable_dt).min(0.25) as f64;
                    self.anim.acc += dt * self.speed as f64;
                    let mut guard = 0;
                    while self.anim.acc >= frames[self.anim.frame].delay as f64 && guard < frames.len() {
                        self.anim.acc -= frames[self.anim.frame].delay as f64;
                        self.anim.frame = (self.anim.frame + 1) % frames.len();
                        guard += 1;
                    }
                    let left = (frames[self.anim.frame].delay as f64 - self.anim.acc) / self.speed as f64;
                    ctx.request_repaint_after_secs(left.max(0.001) as f32);
                }
            }
        }

        // ---- Панели ----
        let media_playing = self.player.as_ref().map(|p| !p.paused).unwrap_or(false)
            || decoded.as_ref().map(|d| matches!(&d.content, Content::Image { frames, .. } if frames.len() > 1) && !self.anim.paused).unwrap_or(false);
        let pointer = ctx.input(|i| i.pointer.hover_pos());
        let over_chrome = pointer.map(|p| self.chrome_rects.iter().any(|r| r.contains(p))).unwrap_or(false);
        let idle = now - self.last_activity;
        let media_paused = self.player.as_ref().map(|p| p.paused).unwrap_or(false);
        let chrome_on = chrome_force
            || idle < CHROME_IDLE
            || over_chrome
            || self.speed_menu
            || self.track_menu
            || self.scrubbing.is_some()
            || !matches!(self.dialog, Dialog::None)
            || media_paused;
        if !chrome_on && media_playing {
            ctx.set_cursor_icon(CursorIcon::None);
        }
        if chrome_on && idle < CHROME_IDLE {
            ctx.request_repaint_after_secs((CHROME_IDLE - idle) as f32 + 0.01);
        }
        let chrome_a = ctx.animate_bool_with_time(Id::new("chrome"), chrome_on, if reduced_motion() { 0.0 } else { M_SMALL });
        self.chrome_rects.clear();

        if cur.is_some() && chrome_a > 0.0 {
            if let Some(d) = &decoded {
                let is_media = matches!(&d.content, Content::Media { .. })
                    || matches!(&d.content, Content::Image { frames, .. } if frames.len() > 1);
                if is_media {
                    self.media_bar(ui, view_rect, d, chrome_a, &mut acts);
                } else {
                    self.capsule(ui, view_rect, d, has_visual, chrome_a, &mut acts);
                }
            } else {
                self.capsule_loading(ui, view_rect, chrome_a, &mut acts);
            }
            if self.files.len() > 1 {
                self.edge_arrows(ui, view_rect, chrome_a, &mut acts);
            }
        }

        // ---- Панель «Сведения» ----
        if info_t > 0.0 {
            let panel = Rect::from_min_max(pos2(view_rect.max.x, full.min.y), pos2(view_rect.max.x + INFO_W, full.max.y));
            self.info_panel(ui, panel, cur.as_deref(), decoded.as_deref(), &mut acts);
        }

        // ---- Тост ----
        self.draw_toast(ui, view_rect, now);

        // ---- Обновление ----
        self.draw_update(ui, view_rect);

        // ---- Диалоги ----
        self.draw_dialog(ui, full, &mut acts);

        // ---- Перетаскивание ----
        let drop_a = ctx.animate_bool_with_time(Id::new("drop"), hovering_files, M_MICRO);
        if drop_a > 0.0 {
            painter.rect_filled(full, 0, Color32::from_black_alpha((170.0 * drop_a) as u8));
            let r = full.shrink(SP_24);
            widgets::viewfinder(&painter, r, 56.0, Stroke::new(3.0, ACCENT.gamma_multiply(drop_a)));
            painter.text(full.center() - vec2(0.0, 12.0), Align2::CENTER_CENTER, tr!("Отпустите, чтобы открыть", "Release to open", "Відпустіть, щоб відкрити"), medium(T_TITLE), TEXT.gamma_multiply(drop_a));
            painter.text(full.center() + vec2(0.0, 20.0), Align2::CENTER_CENTER, tr!("Несколько файлов откроются как подборка", "Multiple files open as a set", "Кілька файлів відкриються як добірка"), light(T_BODY), TEXT_SECONDARY.gamma_multiply(drop_a));
        }

        for a in acts {
            self.apply(&ctx, a, view_rect, ppp, decoded.as_deref());
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.player = None;
        self.settings.save();
    }

    fn clear_color(&self, _v: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 1.0]
    }
}

// ----------------------------------------------------------------------------
// Клавиатура и действия

impl App {
    fn keyboard(&mut self, ctx: &egui::Context, d: Option<&Decoded>, acts: &mut Vec<Act>) {
        if !matches!(self.dialog, Dialog::None) {
            return;
        }
        let is_media = d.map(|d| matches!(d.content, Content::Media { .. })).unwrap_or(false);
        let is_anim = d.map(|d| matches!(&d.content, Content::Image { frames, .. } if frames.len() > 1)).unwrap_or(false);
        let is_pdf = d.map(|d| matches!(d.content, Content::Pdf { .. })).unwrap_or(false);
        let is_text = d.map(|d| matches!(d.content, Content::Text { .. })).unwrap_or(false);
        ctx.input(|i| {
            let m = i.modifiers;
            let p = |k: Key| i.key_pressed(k);
            if m.ctrl {
                if p(Key::O) { acts.push(Act::Open); }
                if p(Key::C) { acts.push(if m.shift { Act::CopyPath } else { Act::Copy }); }
                if p(Key::E) { acts.push(Act::ShowFolder); }
                if p(Key::R) { acts.push(Act::RotateCcw); }
                if p(Key::Num0) { acts.push(Act::ToggleFit(None)); }
                if p(Key::Plus) || p(Key::Equals) { acts.push(Act::ZoomIn); }
                if p(Key::Minus) { acts.push(Act::ZoomOut); }
                return;
            }
            if is_media || is_anim {
                if p(Key::ArrowLeft) { acts.push(if m.shift { Act::Prev } else { Act::SeekRel(-5.0) }); }
                if p(Key::ArrowRight) { acts.push(if m.shift { Act::Next } else { Act::SeekRel(5.0) }); }
                if p(Key::J) { acts.push(Act::SeekRel(-10.0)); }
                if p(Key::L) { acts.push(Act::SeekRel(10.0)); }
                if p(Key::Space) || p(Key::K) { acts.push(Act::PlayPause); }
                if p(Key::Comma) { acts.push(Act::StepBack); }
                if p(Key::Period) { acts.push(Act::StepFwd); }
                if p(Key::M) { acts.push(Act::Mute); }
                if p(Key::ArrowUp) { acts.push(Act::VolumeRel(0.05)); }
                if p(Key::ArrowDown) { acts.push(Act::VolumeRel(-0.05)); }
                if p(Key::OpenBracket) { acts.push(Act::SpeedStep(-1)); }
                if p(Key::CloseBracket) { acts.push(Act::SpeedStep(1)); }
                if p(Key::Backspace) { acts.push(Act::SetSpeed(1.0)); }
                if p(Key::A) { acts.push(Act::TrackCycle); }
                if p(Key::R) && !is_anim { acts.push(Act::Loop); }
                for (k, f) in [(Key::Num0, 0.0), (Key::Num1, 0.1), (Key::Num2, 0.2), (Key::Num3, 0.3), (Key::Num4, 0.4), (Key::Num5, 0.5), (Key::Num6, 0.6), (Key::Num7, 0.7), (Key::Num8, 0.8), (Key::Num9, 0.9)] {
                    if p(k) { acts.push(Act::Seek(-f)); }
                }
            } else if !is_text {
                if p(Key::ArrowLeft) { acts.push(Act::Prev); }
                if p(Key::ArrowRight) { acts.push(Act::Next); }
                if p(Key::Space) { acts.push(Act::Next); }
                if p(Key::Backspace) { acts.push(Act::Prev); }
                if p(Key::Plus) || p(Key::Equals) { acts.push(Act::ZoomIn); }
                if p(Key::Minus) { acts.push(Act::ZoomOut); }
                if p(Key::Num0) { acts.push(Act::ToggleFit(None)); }
                if p(Key::Num1) { acts.push(Act::Actual); }
                if p(Key::R) { acts.push(if m.shift { Act::RotateCcw } else { Act::RotateCw }); }
            }
            if is_pdf {
                if p(Key::ArrowUp) || p(Key::PageUp) { acts.push(Act::PagePrev); }
                if p(Key::ArrowDown) || p(Key::PageDown) { acts.push(Act::PageNext); }
            } else {
                if p(Key::PageUp) { acts.push(Act::Prev); }
                if p(Key::PageDown) { acts.push(Act::Next); }
            }
            if is_text {
                if p(Key::ArrowLeft) && m.alt { acts.push(Act::Prev); }
                if p(Key::ArrowRight) && m.alt { acts.push(Act::Next); }
            }
            if p(Key::Home) && !is_text { acts.push(Act::First); }
            if p(Key::End) && !is_text { acts.push(Act::Last); }
            if p(Key::I) { acts.push(Act::Info); }
            if p(Key::Delete) { acts.push(Act::AskDelete); }
            if p(Key::F) || p(Key::F11) { acts.push(Act::Fullscreen); }
            if p(Key::Enter) && m.alt { acts.push(Act::Fullscreen); }
            if p(Key::F1) || p(Key::Questionmark) { acts.push(Act::Help); }
            if p(Key::Escape) { acts.push(Act::ExitFullscreenOrClose); }
        });
    }

    fn apply(&mut self, ctx: &egui::Context, a: Act, area: Rect, ppp: f32, d: Option<&Decoded>) {
        let size = d.and_then(|d| self.content_size(d));
        let scalable = d.map(|d| matches!(d.content, Content::Pdf { .. })).unwrap_or(false)
            || self.cur().map(|p| matches!(ext_of(p).as_str(), "svg" | "svgz")).unwrap_or(false);
        let fit = size.map(|s| self.fit_scale(s, area, ppp, scalable)).unwrap_or(1.0);
        let anim_frames = d.and_then(|d| match &d.content {
            Content::Image { frames, .. } if frames.len() > 1 => Some(frames.iter().map(|f| f.delay as f64).collect::<Vec<_>>()),
            _ => None,
        });
        match a {
            Act::Prev => self.go(-1),
            Act::Next => self.go(1),
            Act::First => self.set_index(0),
            Act::Last => {
                let n = self.files.len();
                if n > 0 { self.set_index(n - 1) }
            }
            Act::ZoomIn | Act::ZoomOut | Act::ZoomAt(..) => {
                if size.is_none() { return; }
                let (f, at) = match a {
                    Act::ZoomIn => (1.25, None),
                    Act::ZoomOut => (0.8, None),
                    Act::ZoomAt(f, at) => (f, at),
                    _ => unreachable!(),
                };
                let mut ns = self.view.scale * f;
                // Защёлкивание к «вписать» и 100%
                let actual = 1.0 / ppp;
                for snap in [fit, actual] {
                    if (self.view.scale - snap).abs() > 1e-4 && (ns - snap) * (self.view.scale - snap) < 0.0 {
                        ns = snap;
                    }
                }
                if ns <= fit * 1.0001 && f < 1.0 {
                    self.view.fit = true;
                    self.view.scale = fit;
                    self.view.offset = Vec2::ZERO;
                } else {
                    self.zoom_to(ns, at, area, fit, ppp);
                }
            }
            Act::ToggleFit(at) => {
                if size.is_none() { return; }
                if self.view.fit {
                    let actual = 1.0 / ppp;
                    let target = if (fit - actual).abs() < 1e-3 { actual * 2.0 } else { actual };
                    self.zoom_to(target, at, area, fit, ppp);
                } else {
                    self.view.fit = true;
                    self.view.scale = fit;
                    self.view.offset = Vec2::ZERO;
                }
            }
            Act::Actual => {
                if size.is_some() {
                    self.zoom_to(1.0 / ppp, None, area, fit, ppp);
                }
            }
            Act::RotateCw | Act::RotateCcw => {
                self.view.rot = (self.view.rot + if a == Act::RotateCw { 1 } else { 3 }) % 4;
                self.view.fit = true;
                self.view.ready = false;
            }
            Act::Info => self.info_open = !self.info_open,
            Act::AskDelete => {
                if let Some(p) = self.cur().cloned() {
                    self.player = None;
                    self.player_path = None;
                    self.dialog = Dialog::Delete(p);
                }
            }
            Act::Fullscreen => {
                let fs = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
                ctx.send_viewport_cmd(ViewportCommand::Fullscreen(!fs));
            }
            Act::ExitFullscreenOrClose => {
                let fs = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
                if self.speed_menu || self.track_menu {
                    self.speed_menu = false;
                    self.track_menu = false;
                } else if fs {
                    ctx.send_viewport_cmd(ViewportCommand::Fullscreen(false));
                } else if self.info_open {
                    self.info_open = false;
                } else {
                    ctx.send_viewport_cmd(ViewportCommand::Close);
                }
            }
            Act::Copy => self.copy_image(d),
            Act::CopyPath => {
                if let Some(p) = self.cur() {
                    let s = p.to_string_lossy().to_string();
                    if let Ok(mut cb) = arboard::Clipboard::new() {
                        let _ = cb.set_text(s);
                        self.toast(tr!("Путь скопирован", "Path copied", "Шлях скопійовано"));
                    }
                }
            }
            Act::PlayPause => {
                if let Some(pl) = &mut self.player {
                    pl.toggle();
                } else if anim_frames.is_some() {
                    self.anim.paused = !self.anim.paused;
                }
            }
            Act::StepFwd | Act::StepBack => {
                if let Some(pl) = &mut self.player {
                    if a == Act::StepFwd { pl.step_forward() } else { pl.step_back() }
                } else if let Some(fr) = &anim_frames {
                    self.anim.paused = true;
                    self.anim.acc = 0.0;
                    let n = fr.len();
                    self.anim.frame = if a == Act::StepFwd { (self.anim.frame + 1) % n } else { (self.anim.frame + n - 1) % n };
                }
            }
            Act::Seek(t) | Act::SeekRel(t) => {
                if let Some(pl) = &mut self.player {
                    let dur = pl.duration();
                    let target = match a {
                        Act::Seek(t) if t < 0.0 => -t * dur,
                        Act::Seek(t) => t,
                        _ => pl.position() + t,
                    };
                    let target = if dur > 0.0 { target.clamp(0.0, (dur - 0.1).max(0.0)) } else { target.max(0.0) };
                    pl.seek(target);
                } else if let Some(fr) = &anim_frames {
                    let total: f64 = fr.iter().sum();
                    let cur_t: f64 = fr[..self.anim.frame].iter().sum::<f64>() + self.anim.acc;
                    let target = match a {
                        Act::Seek(t) if t < 0.0 => -t * total,
                        Act::Seek(t) => t,
                        _ => (cur_t + t).rem_euclid(total.max(0.001)),
                    };
                    let mut acc = 0.0;
                    for (i, dl) in fr.iter().enumerate() {
                        if acc + dl > target || i == fr.len() - 1 {
                            self.anim.frame = i;
                            self.anim.acc = (target - acc).max(0.0).min(*dl);
                            break;
                        }
                        acc += dl;
                    }
                }
            }
            Act::Mute => {
                self.settings.muted = !self.settings.muted;
                self.apply_volume();
            }
            Act::Volume(v) => {
                self.settings.volume = v.clamp(0.0, 1.0);
                self.settings.muted = false;
                self.apply_volume();
            }
            Act::VolumeRel(dv) => {
                self.settings.volume = (self.settings.volume + dv).clamp(0.0, 1.0);
                self.settings.muted = false;
                self.apply_volume();
                self.toast(&trf!(["Громкость {:.0}%", "Volume {:.0}%", "Гучність {:.0}%"], self.settings.volume * 100.0));
            }
            Act::Loop => {
                self.settings.loop_video = !self.settings.loop_video;
                if let Some(pl) = &mut self.player {
                    pl.looping = self.settings.loop_video;
                }
                self.toast(if self.settings.loop_video { tr!("Повтор включён", "Loop on", "Повтор увімкнено") } else { tr!("Повтор выключен", "Loop off", "Повтор вимкнено") });
            }
            Act::SpeedMenu => {
                self.speed_menu = !self.speed_menu;
                self.track_menu = false;
            }
            Act::TrackMenu => {
                self.track_menu = !self.track_menu;
                self.speed_menu = false;
            }
            Act::SetTrack(t) => {
                if let Some(pl) = &mut self.player {
                    pl.set_audio_track(t);
                }
                self.track_menu = false;
            }
            Act::TrackCycle => {
                let msg = self.player.as_mut().and_then(|pl| {
                    let n = pl.info.audio_tracks.len();
                    if n < 2 {
                        return None;
                    }
                    let t = (pl.audio_track + 1) % n;
                    pl.set_audio_track(t);
                    Some(trf!(["Звук: {}", "Audio: {}", "Звук: {}"], pl.info.audio_tracks[t].label()))
                });
                if let Some(m) = msg {
                    self.toast(&m);
                }
            }
            Act::SetSpeed(s) => {
                self.speed = s.clamp(0.25, 3.0);
                if let Some(pl) = &mut self.player {
                    pl.set_speed(self.speed);
                }
                self.speed_menu = false;
            }
            Act::SpeedStep(dir) => {
                let i = SPEEDS.iter().position(|s| (*s - self.speed).abs() < 1e-3).unwrap_or(3) as i32;
                let j = (i + dir).clamp(0, SPEEDS.len() as i32 - 1) as usize;
                self.speed = SPEEDS[j];
                if let Some(pl) = &mut self.player {
                    pl.set_speed(self.speed);
                }
                self.toast(&trf!(["Скорость {}", "Speed {}", "Швидкість {}"], fmt_speed(self.speed)));
            }
            Act::PagePrev | Act::PageNext => {
                if let Some(Decoded { content: Content::Pdf { pages, .. }, .. }) = d {
                    let np = if a == Act::PageNext { (self.pdf_page + 1).min(pages - 1) } else { self.pdf_page.saturating_sub(1) };
                    if np != self.pdf_page {
                        self.pdf_page = np;
                        self.view = View::default();
                    }
                }
            }
            Act::OpenWith => {
                if let Some(p) = self.cur() { winutil::open_with_dialog(p) }
            }
            Act::ShowFolder => {
                if let Some(p) = self.cur() { winutil::show_in_explorer(p) }
            }
            Act::Open => {
                let mut dlg = rfd::FileDialog::new().set_title(tr!("Открыть в JustView", "Open in JustView", "Відкрити в JustView"));
                if let Some(dir) = self.cur().and_then(|p| p.parent()) {
                    dlg = dlg.set_directory(dir);
                }
                if let Some(p) = dlg.pick_file() {
                    self.pending_open = Some(p);
                }
            }
            Act::MakeDefault => match register::register() {
                Ok(()) => {
                    register::open_default_apps();
                    self.toast(tr!("JustView зарегистрирован. Нажмите «Установить по умолчанию» в Параметрах", "JustView is registered. Click “Set default” in Settings", "JustView зареєстровано. Натисніть «Установити за замовчуванням» у Параметрах"));
                }
                Err(e) => self.toast(&trf!(["Не удалось зарегистрировать: {e}", "Registration failed: {e}", "Не вдалося зареєструвати: {e}"])),
            },
            Act::Help => {
                self.dialog = Dialog::Help;
            }
        }
        ctx.request_repaint();
    }

    fn copy_image(&mut self, d: Option<&Decoded>) {
        let img: Option<Arc<ColorImage>> = match d.map(|d| &d.content) {
            Some(Content::Image { frames, .. }) => Some(frames[self.anim.frame.min(frames.len() - 1)].image.clone()),
            Some(Content::Media { .. }) => self.player.as_ref().and_then(|p| p.current.as_ref().map(|f| f.image.clone())),
            Some(Content::Pdf { first, .. }) => Some(first.clone()),
            _ => None,
        };
        let Some(img) = img else {
            self.toast(tr!("Здесь нечего копировать", "Nothing to copy here", "Тут нічого копіювати"));
            return;
        };
        let mut bytes = Vec::with_capacity(img.pixels.len() * 4);
        for c in &img.pixels {
            bytes.extend_from_slice(&c.to_srgba_unmultiplied());
        }
        let data = arboard::ImageData { width: img.size[0], height: img.size[1], bytes: bytes.into() };
        match arboard::Clipboard::new().and_then(|mut c| c.set_image(data)) {
            Ok(()) => self.toast(tr!("Изображение скопировано", "Image copied", "Зображення скопійовано")),
            Err(e) => self.toast(&trf!(["Не удалось скопировать: {e}", "Copy failed: {e}", "Не вдалося скопіювати: {e}"])),
        }
    }

    fn delete_confirmed(&mut self, path: PathBuf) {
        match winutil::recycle(&path) {
            Ok(()) => {
                if let Some(i) = self.files.iter().position(|p| *p == path) {
                    self.files.remove(i);
                    self.cache.remove(&path);
                    self.tex.remove(&path);
                    self.last_drawn = None;
                    if self.files.is_empty() {
                        self.index = 0;
                    } else {
                        let ni = i.min(self.files.len() - 1);
                        self.set_index(ni);
                        self.fade_from = None;
                    }
                }
                self.toast(tr!("Файл перемещён в Корзину", "File moved to the Recycle Bin", "Файл переміщено до Кошика"));
            }
            Err(e) => {
                self.player_path = None;
                self.toast(&trf!(["Не удалось удалить: {e}", "Delete failed: {e}", "Не вдалося видалити: {e}"]));
            }
        }
    }
}

// ----------------------------------------------------------------------------
// Панели управления

enum Item {
    Btn(Icon, Act, String, bool),
    Text(String, f32, Option<Act>, String),
    Sep,
}

impl App {
    fn layout_items(&mut self, ui: &mut Ui, center_x: f32, bottom: f32, items: Vec<Item>, alpha: f32, acts: &mut Vec<Act>) -> Rect {
        let gap = SP_4;
        let pad = SP_8;
        let w_of = |it: &Item| match it {
            Item::Btn(..) => ICON_BTN,
            Item::Text(_, w, ..) => *w,
            Item::Sep => 1.0 + SP_8,
        };
        let total: f32 = items.iter().map(w_of).sum::<f32>() + gap * (items.len().saturating_sub(1)) as f32 + pad * 2.0;
        let rect = Rect::from_min_size(pos2(center_x - total / 2.0, bottom - CAPSULE_H), vec2(total, CAPSULE_H));
        self.chrome_rects.push(rect);
        ui.interact(rect, Id::new("capsule_bg"), Sense::click()); // клики по фону не уходят на холст
        let painter = ui.painter().clone();
        painter.rect_filled(rect, (CAPSULE_H / 2.0) as u8, GLASS.gamma_multiply(alpha));
        let mut x = rect.left() + pad;
        let cy = rect.center().y;
        for (i, it) in items.into_iter().enumerate() {
            let w = w_of(&it);
            let r = Rect::from_min_size(pos2(x, cy - ICON_BTN / 2.0), vec2(w, ICON_BTN));
            match it {
                Item::Btn(icon, act, tip, enabled) => {
                    if icon_button(ui, Id::new(("cap", i, format!("{icon:?}"))), r, icon, &tip, enabled, alpha).clicked() {
                        acts.push(act);
                    }
                }
                Item::Text(t, _, act, tip) => {
                    let id = Id::new(("captext", i));
                    if let Some(act) = act {
                        let resp = ui.interact(r, id, Sense::click());
                        let hv = ui.ctx().animate_bool_with_time(id.with("h"), resp.hovered(), M_MICRO);
                        if hv > 0.0 {
                            painter.rect_filled(r, (ICON_BTN / 2.0) as u8, SURFACE_RAISED.gamma_multiply(hv * alpha));
                        }
                        if resp.hovered() {
                            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                        }
                        let resp = if tip.is_empty() { resp } else { resp.on_hover_text(tip) };
                        if resp.clicked() {
                            acts.push(act);
                        }
                    }
                    painter.text(r.center(), Align2::CENTER_CENTER, t, regular(T_FOOTNOTE), TEXT_SECONDARY.gamma_multiply(alpha));
                }
                Item::Sep => {
                    let lx = r.center().x;
                    painter.line_segment([pos2(lx, cy - 10.0), pos2(lx, cy + 10.0)], Stroke::new(1.0, HAIRLINE.gamma_multiply(alpha)));
                }
            }
            x += w + gap;
        }
        rect
    }

    fn counter(&self) -> Option<String> {
        (self.files.len() > 1).then(|| format!("{} / {}", self.index + 1, self.files.len()))
    }

    fn capsule(&mut self, ui: &mut Ui, area: Rect, d: &Decoded, has_visual: bool, alpha: f32, acts: &mut Vec<Act>) {
        let ppp = ui.ctx().pixels_per_point();
        let mut items = Vec::new();
        if let Some(c) = self.counter() {
            let w = (c.len() as f32 * 7.5 + 16.0).max(52.0);
            items.push(Item::Text(c, w, None, String::new()));
            items.push(Item::Sep);
        }
        if let Content::Pdf { pages, .. } = &d.content {
            items.push(Item::Btn(Icon::ChevronUp, Act::PagePrev, tr!("Предыдущая страница  ↑", "Previous page  ↑", "Попередня сторінка  ↑").into(), self.pdf_page > 0));
            items.push(Item::Text(trf!(["{} из {}", "{} of {}", "{} з {}"], self.pdf_page + 1, pages), 64.0, None, String::new()));
            items.push(Item::Btn(Icon::ChevronDown, Act::PageNext, tr!("Следующая страница  ↓", "Next page  ↓", "Наступна сторінка  ↓").into(), self.pdf_page + 1 < *pages));
            items.push(Item::Sep);
        }
        if has_visual {
            let pct = format!("{:.0}%", self.view.scale * ppp * 100.0);
            items.push(Item::Btn(Icon::ZoomOut, Act::ZoomOut, tr!("Уменьшить  −", "Zoom out  −", "Зменшити  −").into(), true));
            items.push(Item::Text(pct, 56.0, Some(Act::ToggleFit(None)), if self.view.fit { tr!("Масштаб 100%  0", "Zoom 100%  0", "Масштаб 100%  0").into() } else { tr!("Вписать в окно  0", "Fit to window  0", "Вписати у вікно  0").into() }));
            items.push(Item::Btn(Icon::ZoomIn, Act::ZoomIn, tr!("Увеличить  +", "Zoom in  +", "Збільшити  +").into(), true));
            items.push(Item::Sep);
            if matches!(d.content, Content::Image { .. }) {
                items.push(Item::Btn(Icon::Rotate, Act::RotateCw, tr!("Повернуть  R", "Rotate  R", "Повернути  R").into(), true));
                items.push(Item::Btn(Icon::Copy, Act::Copy, tr!("Копировать изображение  Ctrl+C", "Copy image  Ctrl+C", "Копіювати зображення  Ctrl+C").into(), true));
            }
        } else {
            items.push(Item::Btn(Icon::OpenWith, Act::OpenWith, tr!("Открыть с помощью…", "Open with…", "Відкрити за допомогою…").into(), true));
        }
        items.push(Item::Btn(Icon::Info, Act::Info, tr!("Сведения  I", "Info  I", "Відомості  I").into(), true));
        items.push(Item::Btn(Icon::Trash, Act::AskDelete, tr!("Удалить  Del", "Delete  Del", "Видалити  Del").into(), true));
        let fs = ui.ctx().input(|i| i.viewport().fullscreen.unwrap_or(false));
        items.push(Item::Btn(
            if fs { Icon::ExitFullscreen } else { Icon::Fullscreen },
            Act::Fullscreen,
            if fs { tr!("Выйти из полноэкранного режима  F", "Exit full screen  F", "Вийти з повноекранного режиму  F").into() } else { tr!("Во весь экран  F", "Full screen  F", "На весь екран  F").into() },
            true,
        ));
        self.layout_items(ui, area.center().x, area.bottom() - SP_20, items, alpha, acts);
    }

    fn capsule_loading(&mut self, ui: &mut Ui, area: Rect, alpha: f32, acts: &mut Vec<Act>) {
        let mut items = Vec::new();
        if let Some(c) = self.counter() {
            let w = (c.len() as f32 * 7.5 + 16.0).max(52.0);
            items.push(Item::Text(c, w, None, String::new()));
            items.push(Item::Sep);
        }
        items.push(Item::Btn(Icon::Info, Act::Info, tr!("Сведения  I", "Info  I", "Відомості  I").into(), true));
        items.push(Item::Btn(Icon::Trash, Act::AskDelete, tr!("Удалить  Del", "Delete  Del", "Видалити  Del").into(), true));
        self.layout_items(ui, area.center().x, area.bottom() - SP_20, items, alpha, acts);
    }

    fn edge_arrows(&mut self, ui: &mut Ui, area: Rect, alpha: f32, acts: &mut Vec<Act>) {
        let pointer = ui.ctx().input(|i| i.pointer.hover_pos());
        for (side, icon, act, tip) in [(0, Icon::ChevronLeft, Act::Prev, tr!("Предыдущий  ←", "Previous  ←", "Попередній  ←")), (1, Icon::ChevronRight, Act::Next, tr!("Следующий  →", "Next  →", "Наступний  →"))] {
            let zone = if side == 0 {
                Rect::from_min_max(area.min, pos2(area.left() + 160.0, area.bottom() - 120.0))
            } else {
                Rect::from_min_max(pos2(area.right() - 160.0, area.top()), pos2(area.right(), area.bottom() - 120.0))
            };
            let near = pointer.map(|p| zone.contains(p)).unwrap_or(false);
            let a = ui.ctx().animate_bool_with_time(Id::new(("edge", side)), near, M_MICRO) * alpha;
            if a <= 0.01 {
                continue;
            }
            let cx = if side == 0 { area.left() + SP_16 + EDGE_BTN / 2.0 } else { area.right() - SP_16 - EDGE_BTN / 2.0 };
            let r = Rect::from_center_size(pos2(cx, area.center().y), Vec2::splat(EDGE_BTN));
            self.chrome_rects.push(r);
            ui.painter().circle_filled(r.center(), EDGE_BTN / 2.0, GLASS.gamma_multiply(a));
            if icon_button(ui, Id::new(("edgebtn", side)), r.shrink(4.0), icon, tip, true, a).clicked() {
                acts.push(act);
            }
        }
    }

    fn media_bar(&mut self, ui: &mut Ui, area: Rect, d: &Decoded, alpha: f32, acts: &mut Vec<Act>) {
        let ctx = ui.ctx().clone();
        let painter = ui.painter().clone();
        let w = (area.width() - SP_48).clamp(320.0, 760.0);
        let h = 96.0;
        let rect = Rect::from_min_size(pos2(area.center().x - w / 2.0, area.bottom() - SP_20 - h), vec2(w, h));
        self.chrome_rects.push(rect);
        ui.interact(rect, Id::new("mediabar_bg"), Sense::click());
        painter.rect_filled(rect, R_CARD, GLASS.gamma_multiply(alpha));

        // Состояние источника
        let (pos, dur, paused, has_audio, is_anim, frame_based) = if let Some(pl) = &self.player {
            (pl.position(), pl.duration(), pl.paused, pl.info.has_audio, false, pl.has_video())
        } else if let Content::Image { frames, .. } = &d.content {
            let total: f64 = frames.iter().map(|f| f.delay as f64).sum();
            let t: f64 = frames[..self.anim.frame.min(frames.len())].iter().map(|f| f.delay as f64).sum::<f64>() + self.anim.acc;
            (t, total, self.anim.paused, false, true, true)
        } else {
            (0.0, 0.0, true, false, false, false)
        };

        // Ряд 1: время + шкала
        let row1 = Rect::from_min_size(rect.min + vec2(SP_16, SP_12), vec2(w - SP_32, 24.0));
        let tw = 56.0;
        let shown_pos = if self.scrubbing.is_some() { self.player.as_ref().map(|p| p.position()).unwrap_or(pos) } else { pos };
        let n_frames = match &d.content { Content::Image { frames, .. } => frames.len(), _ => 0 };
        let pos_text = if is_anim { format!("{} / {}", self.anim.frame + 1, n_frames) } else { fmt_time(shown_pos) };
        painter.text(pos2(row1.left(), row1.center().y), Align2::LEFT_CENTER, pos_text, regular(T_FOOTNOTE), TEXT.gamma_multiply(alpha));
        let dur_text = if is_anim { trf!(["{:.1} с", "{:.1} s", "{:.1} с"], dur).replace('.', ",") } else { fmt_time(dur) };
        painter.text(pos2(row1.right(), row1.center().y), Align2::RIGHT_CENTER, dur_text, regular(T_FOOTNOTE), TEXT_SECONDARY.gamma_multiply(alpha));
        let bar = Rect::from_min_max(pos2(row1.left() + tw, row1.top()), pos2(row1.right() - tw - 8.0, row1.bottom()));
        let seek = ui.interact(bar, Id::new("seek"), Sense::click_and_drag());
        let hv = ctx.animate_bool_with_time(Id::new("seek_h"), seek.hovered() || seek.dragged(), M_MICRO);
        let th = 4.0 + 2.0 * hv;
        let track = Rect::from_center_size(bar.center(), vec2(bar.width(), th));
        painter.rect_filled(track, (th / 2.0) as u8, Color32::from_white_alpha((38.0 * alpha) as u8));
        let frac = if dur > 0.0 { (shown_pos / dur).clamp(0.0, 1.0) as f32 } else { 0.0 };
        let mut played = track;
        played.max.x = track.left() + track.width() * frac;
        painter.rect_filled(played, (th / 2.0) as u8, ACCENT.gamma_multiply(alpha));
        if hv > 0.0 {
            painter.circle_filled(pos2(played.max.x, track.center().y), 6.0 * hv, TEXT.gamma_multiply(alpha));
        }
        if seek.hovered() {
            ctx.set_cursor_icon(CursorIcon::PointingHand);
            if let Some(p) = seek.hover_pos() {
                let t = ((p.x - bar.left()) / bar.width()).clamp(0.0, 1.0) as f64 * dur;
                let tip_pos = pos2(p.x, bar.top() - SP_12);
                let g = painter.layout_no_wrap(fmt_time(t), regular(T_CAPTION), TEXT);
                let tr = Rect::from_center_size(tip_pos, g.size() + vec2(16.0, 8.0));
                painter.rect_filled(tr, (tr.height() / 2.0) as u8, SURFACE_RAISED);
                painter.galley(tr.center() - g.size() / 2.0, g, TEXT);
            }
        }
        if dur > 0.0 {
            let t_at = |x: f32| ((x - bar.left()) / bar.width()).clamp(0.0, 1.0) as f64 * dur;
            if seek.drag_started() {
                let was_playing = !paused;
                self.scrubbing = Some(was_playing);
                if let Some(pl) = &mut self.player {
                    pl.set_paused(true);
                }
            }
            if seek.dragged() {
                if let Some(p) = seek.interact_pointer_pos() {
                    let t = t_at(p.x);
                    if let Some(pl) = &mut self.player {
                        pl.scrub(t);
                    } else {
                        acts.push(Act::Seek(t));
                    }
                }
            }
            if seek.drag_stopped() {
                let resume = self.scrubbing.take().unwrap_or(false);
                if let Some(p) = seek.interact_pointer_pos() {
                    let t = t_at(p.x);
                    if let Some(pl) = &mut self.player {
                        pl.end_scrub(t, resume);
                    } else {
                        acts.push(Act::Seek(t));
                    }
                }
            } else if seek.clicked() {
                if let Some(p) = seek.interact_pointer_pos() {
                    acts.push(Act::Seek(t_at(p.x)));
                }
            }
        }

        // Ряд 2: кнопки
        let cy = rect.bottom() - SP_8 - ICON_BTN / 2.0 - 2.0;
        let mut x = rect.left() + SP_8;
        let btn = |ui: &mut Ui, x: &mut f32, icon: Icon, act: Act, tip: &str, en: bool, acts: &mut Vec<Act>| {
            let r = Rect::from_center_size(pos2(*x + ICON_BTN / 2.0, cy), Vec2::splat(ICON_BTN));
            if icon_button(ui, Id::new(("mb", format!("{icon:?}"))), r, icon, tip, en, alpha).clicked() {
                acts.push(act);
            }
            *x += ICON_BTN + SP_4;
        };
        let ended = self.player.as_ref().map(|p| p.ended).unwrap_or(false);
        let (pi, pt) = if ended {
            (Icon::Replay, tr!("Сначала  Пробел", "Replay  Space", "Спочатку  Пробіл"))
        } else if paused {
            (Icon::Play, tr!("Воспроизвести  Пробел", "Play  Space", "Відтворити  Пробіл"))
        } else {
            (Icon::Pause, tr!("Пауза  Пробел", "Pause  Space", "Пауза  Пробіл"))
        };
        btn(ui, &mut x, pi, Act::PlayPause, pt, true, acts);
        if frame_based {
            btn(ui, &mut x, Icon::StepBack, Act::StepBack, tr!("Кадр назад  ,", "Previous frame  ,", "Кадр назад  ,"), true, acts);
            btn(ui, &mut x, Icon::StepFwd, Act::StepFwd, tr!("Кадр вперёд  .", "Next frame  .", "Кадр уперед  ."), true, acts);
        }
        if has_audio {
            x += SP_8;
            let vol = self.settings.volume;
            let vi = if self.settings.muted || vol <= 0.001 { Icon::Mute } else if vol < 0.5 { Icon::VolumeLow } else { Icon::Volume };
            btn(ui, &mut x, vi, Act::Mute, if self.settings.muted { tr!("Включить звук  M", "Unmute  M", "Увімкнути звук  M") } else { tr!("Выключить звук  M", "Mute  M", "Вимкнути звук  M") }, true, acts);
            let sr = Rect::from_min_size(pos2(x, cy - 10.0), vec2(88.0, 20.0));
            let shown = if self.settings.muted { 0.0 } else { vol };
            let (resp, nv) = widgets::slider(ui, Id::new("vol"), sr, shown, alpha);
            if let Some(v) = nv {
                acts.push(Act::Volume(v));
            }
            if resp.hovered() {
                let sc = ctx.input(|i| i.smooth_scroll_delta.y);
                if sc.abs() > 0.1 {
                    acts.push(Act::Volume(vol + sc / 1000.0));
                }
            }
            let _ = resp.on_hover_text(trf!(["Громкость {:.0}%  ↑↓", "Volume {:.0}%  ↑↓", "Гучність {:.0}%  ↑↓"], shown * 100.0));
        }

        // Правая группа (справа налево)
        let fs = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
        let mut rx = rect.right() - SP_8;
        let rbtn = |ui: &mut Ui, rx: &mut f32, icon: Icon, act: Act, tip: &str, acts: &mut Vec<Act>| {
            let r = Rect::from_center_size(pos2(*rx - ICON_BTN / 2.0, cy), Vec2::splat(ICON_BTN));
            if icon_button(ui, Id::new(("mbr", format!("{icon:?}"))), r, icon, tip, true, alpha).clicked() {
                acts.push(act);
            }
            *rx -= ICON_BTN + SP_4;
        };
        rbtn(ui, &mut rx, if fs { Icon::ExitFullscreen } else { Icon::Fullscreen }, Act::Fullscreen, tr!("Во весь экран  F", "Full screen  F", "На весь екран  F"), acts);
        rbtn(ui, &mut rx, Icon::Info, Act::Info, tr!("Сведения  I", "Info  I", "Відомості  I"), acts);
        rbtn(ui, &mut rx, Icon::Trash, Act::AskDelete, tr!("Удалить  Del", "Delete  Del", "Видалити  Del"), acts);
        if !is_anim {
            let looping = self.settings.loop_video;
            let r = Rect::from_center_size(pos2(rx - ICON_BTN / 2.0, cy), Vec2::splat(ICON_BTN));
            if icon_button(ui, Id::new("loop"), r, Icon::Loop, if looping { tr!("Повтор: вкл  R", "Loop: on  R", "Повтор: увімк  R") } else { tr!("Повтор: выкл  R", "Loop: off  R", "Повтор: вимк  R") }, true, alpha).clicked() {
                acts.push(Act::Loop);
            }
            if looping {
                ui.painter().circle_filled(pos2(r.center().x, r.bottom() - 1.0), 2.0, ACCENT.gamma_multiply(alpha));
            }
            rx -= ICON_BTN + SP_4;
        }
        if let Some(c) = self.counter() {
            painter.text(pos2(rx - SP_8, cy), Align2::RIGHT_CENTER, &c, regular(T_FOOTNOTE), TEXT_SECONDARY.gamma_multiply(alpha));
            let g = painter.layout_no_wrap(c, regular(T_FOOTNOTE), TEXT);
            rx -= g.size().x + SP_16;
        }

        // Скорость 0.25–3×
        let label = fmt_speed(self.speed);
        let sw = 64.0;
        let sr = Rect::from_center_size(pos2(rx - sw / 2.0 - SP_4, cy), vec2(sw, 30.0));
        let resp = ui.interact(sr, Id::new("speed"), Sense::click());
        let scale = widgets::press_scale(ui, Id::new("speed_press"), &resp, sr.size());
        let hv = ctx.animate_bool_with_time(Id::new("speed_h"), resp.hovered() || self.speed_menu, M_MICRO);
        let srr = Rect::from_center_size(sr.center(), sr.size() * scale);
        painter.rect_filled(srr, 15, SURFACE_RAISED.gamma_multiply(alpha * (0.6 + 0.4 * hv)));
        let non_default = (self.speed - 1.0).abs() > 1e-3;
        painter.text(srr.center(), Align2::CENTER_CENTER, &label, regular(T_FOOTNOTE), if non_default { ACCENT.gamma_multiply(alpha) } else { TEXT.gamma_multiply(alpha) });
        if resp.hovered() {
            ctx.set_cursor_icon(CursorIcon::PointingHand);
            let sc = ctx.input(|i| i.smooth_scroll_delta.y);
            if sc.abs() > 20.0 {
                acts.push(Act::SpeedStep(if sc > 0.0 { 1 } else { -1 }));
            }
        }
        if resp.clicked() {
            acts.push(Act::SpeedMenu);
        }
        let resp = resp.on_hover_text(tr!("Скорость  [ ]", "Speed  [ ]", "Швидкість  [ ]"));
        let _ = resp;

        // Звуковая дорожка — слева от скорости, только если дорожек несколько
        let tracks: Vec<String> = self.player.as_ref().map(|p| p.info.audio_tracks.iter().map(|t| t.label()).collect()).unwrap_or_default();
        let cur_track = self.player.as_ref().map(|p| p.audio_track).unwrap_or(0);
        let mut track_rect: Option<Rect> = None;
        if tracks.len() > 1 {
            let full_name = tracks[cur_track.min(tracks.len() - 1)].clone();
            let short = short_track(&full_name);
            let gw = painter.layout_no_wrap(short.clone(), regular(T_FOOTNOTE), TEXT).size().x;
            let tw = (gw + SP_16 + 10.0 + SP_12).clamp(72.0, 168.0);
            let right = sr.left() - SP_8;
            let tr = Rect::from_center_size(pos2(right - tw / 2.0, cy), vec2(tw, 30.0));
            if tr.left() > x + SP_8 {
                track_rect = Some(tr);
                let resp = ui.interact(tr, Id::new("track"), Sense::click());
                let scale = widgets::press_scale(ui, Id::new("track_press"), &resp, tr.size());
                let hv = ctx.animate_bool_with_time(Id::new("track_h"), resp.hovered() || self.track_menu, M_MICRO);
                let trr = Rect::from_center_size(tr.center(), tr.size() * scale);
                painter.rect_filled(trr, 15, SURFACE_RAISED.gamma_multiply(alpha * (0.6 + 0.4 * hv)));
                // Значок дорожек: три черты разной высоты
                let ic = pos2(trr.left() + SP_16, trr.center().y);
                for (i, hh) in [6.0f32, 10.0, 7.0].iter().enumerate() {
                    let xx = ic.x - 4.0 + i as f32 * 4.0;
                    painter.line_segment([pos2(xx, ic.y - hh / 2.0), pos2(xx, ic.y + hh / 2.0)], Stroke::new(1.5, TEXT_SECONDARY.gamma_multiply(alpha)));
                }
                let text_left = trr.left() + SP_16 + 10.0;
                let shown = ellipsize(&painter, &short, T_FOOTNOTE, trr.right() - SP_12 - text_left);
                painter.text(pos2(text_left, trr.center().y), Align2::LEFT_CENTER, shown, regular(T_FOOTNOTE), TEXT.gamma_multiply(alpha));
                if resp.hovered() {
                    ctx.set_cursor_icon(CursorIcon::PointingHand);
                }
                if resp.clicked() {
                    acts.push(Act::TrackMenu);
                }
                let _ = resp.on_hover_text(trf!(["Звуковая дорожка: {full_name}  A", "Audio track: {full_name}  A", "Звукова доріжка: {full_name}  A"]));
            }
        }
        let track_open = ease(ctx.animate_bool_with_time(Id::new("track_menu"), self.track_menu && track_rect.is_some(), if reduced_motion() { 0.0 } else { M_SMALL }));
        if let (Some(tr), true) = (track_rect, track_open > 0.0) {
            let row_h = 34.0;
            let widest = tracks.iter().map(|t| painter.layout_no_wrap(t.clone(), regular(T_SUBHEAD), TEXT).size().x).fold(0.0f32, f32::max);
            let mw = (widest + SP_32 + SP_24).clamp(200.0, 360.0);
            let mh = row_h * tracks.len() as f32 + SP_8 * 2.0;
            let lift = (1.0 - track_open) * 8.0;
            let left = (tr.center().x - mw / 2.0).min(rect.right() - mw).max(rect.left());
            let mr = Rect::from_min_size(pos2(left, rect.top() - SP_8 - mh + lift), vec2(mw, mh));
            self.chrome_rects.push(mr);
            let a = track_open * alpha;
            ui.interact(mr, Id::new("track_menu_bg"), Sense::click());
            painter.rect_filled(mr, R_FIELD, SURFACE.gamma_multiply(a));
            for (i, name) in tracks.iter().enumerate() {
                let r = Rect::from_min_size(pos2(mr.left() + SP_4, mr.top() + SP_8 + i as f32 * row_h), vec2(mw - SP_8, row_h));
                let id = Id::new(("track_row", i));
                let rr = ui.interact(r, id, Sense::click());
                let h = ctx.animate_bool_with_time(id.with("h"), rr.hovered(), M_MICRO);
                if h > 0.0 {
                    painter.rect_filled(r, 8, SURFACE_RAISED.gamma_multiply(h * a));
                }
                let sel = i == cur_track;
                if sel {
                    let c = pos2(r.left() + SP_16, r.center().y);
                    painter.add(Shape::line(vec![c + vec2(-4.0, 0.0), c + vec2(-1.0, 3.5), c + vec2(5.0, -4.0)], Stroke::new(1.5, ACCENT.gamma_multiply(a))));
                }
                let txt = ellipsize(&painter, name, T_SUBHEAD, r.right() - SP_12 - (r.left() + SP_32));
                let col = if sel { ACCENT } else { TEXT };
                painter.text(pos2(r.left() + SP_32, r.center().y), Align2::LEFT_CENTER, txt, regular(T_SUBHEAD), col.gamma_multiply(a));
                if rr.hovered() {
                    ctx.set_cursor_icon(CursorIcon::PointingHand);
                }
                if rr.clicked() && self.track_menu {
                    acts.push(Act::SetTrack(i));
                }
            }
            if self.track_menu && ctx.input(|i| i.pointer.any_pressed()) {
                if let Some(p) = ctx.input(|i| i.pointer.interact_pos()) {
                    if !mr.contains(p) && !tr.contains(p) {
                        self.track_menu = false;
                    }
                }
            }
        }

        // Меню скоростей растёт из кнопки вверх
        let open_t = ease(ctx.animate_bool_with_time(Id::new("speed_menu"), self.speed_menu, if reduced_motion() { 0.0 } else { M_SMALL }));
        if open_t > 0.0 {
            let row_h = 30.0;
            let mh = row_h * SPEEDS.len() as f32 + SP_8 * 2.0;
            let mw = 112.0;
            let lift = (1.0 - open_t) * 8.0;
            let mr = Rect::from_min_size(pos2(sr.center().x - mw / 2.0, rect.top() - SP_8 - mh + lift), vec2(mw, mh));
            self.chrome_rects.push(mr);
            let a = open_t * alpha;
            ui.interact(mr, Id::new("speed_menu_bg"), Sense::click());
            painter.rect_filled(mr, R_FIELD, SURFACE.gamma_multiply(a));
            for (i, s) in SPEEDS.iter().enumerate().rev() {
                let ri = SPEEDS.len() - 1 - i;
                let r = Rect::from_min_size(pos2(mr.left() + SP_4, mr.top() + SP_8 + ri as f32 * row_h), vec2(mw - SP_8, row_h));
                let id = Id::new(("speed_row", i));
                let rr = ui.interact(r, id, Sense::click());
                let h = ctx.animate_bool_with_time(id.with("h"), rr.hovered(), M_MICRO);
                if h > 0.0 {
                    painter.rect_filled(r, 8, SURFACE_RAISED.gamma_multiply(h * a));
                }
                let sel = (*s - self.speed).abs() < 1e-3;
                let col = if sel { ACCENT } else { TEXT };
                if sel {
                    let c = pos2(r.left() + SP_16, r.center().y);
                    painter.add(Shape::line(vec![c + vec2(-4.0, 0.0), c + vec2(-1.0, 3.5), c + vec2(5.0, -4.0)], Stroke::new(1.5, ACCENT.gamma_multiply(a))));
                }
                let txt = if *s == 1.0 { tr!("1× обычная", "1× normal", "1× звичайна").to_string() } else { fmt_speed(*s) };
                painter.text(pos2(r.left() + SP_32, r.center().y), Align2::LEFT_CENTER, txt, regular(T_SUBHEAD), col.gamma_multiply(a));
                if rr.hovered() {
                    ctx.set_cursor_icon(CursorIcon::PointingHand);
                }
                if rr.clicked() && self.speed_menu {
                    acts.push(Act::SetSpeed(*s));
                }
            }
            // Клик мимо меню закрывает его
            if self.speed_menu && ctx.input(|i| i.pointer.any_pressed()) {
                if let Some(p) = ctx.input(|i| i.pointer.interact_pos()) {
                    if !mr.contains(p) && !sr.contains(p) {
                        self.speed_menu = false;
                    }
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // Содержимое без изображения

    fn draw_audio(&mut self, ui: &mut Ui, area: Rect, info: &crate::ffmpeg::MediaInfo, has_cover: bool, path: &Path, acts: &mut Vec<Act>) {
        let painter = ui.painter().clone();
        let center = area.center() - vec2(0.0, 48.0);
        let title = if info.title.is_empty() {
            path.file_stem().unwrap_or_default().to_string_lossy().to_string()
        } else {
            info.title.clone()
        };
        let sub: Vec<&str> = [info.artist.as_str(), info.album.as_str()].into_iter().filter(|s| !s.is_empty()).collect();
        let y;
        if has_cover {
            // Обложка рисуется общим путём (как изображение) — подпись ниже неё.
            let cover_h = self.last_drawn.as_ref().map(|d| d.rect.height()).unwrap_or(0.0);
            y = (area.center().y + cover_h / 2.0 + SP_24).min(area.bottom() - 150.0);
        } else {
            // Волна вместо обложки: прослушанная часть — светлая; клик — перемотка.
            let wr = Rect::from_center_size(center, vec2((area.width() - 96.0).min(640.0), 120.0));
            let pos = self.player.as_ref().map(|p| p.position()).unwrap_or(0.0);
            let dur = self.player.as_ref().map(|p| p.duration()).unwrap_or(0.0);
            let frac = if dur > 0.0 { (pos / dur) as f32 } else { 0.0 };
            let wave = self.player.as_ref().and_then(|p| p.waveform.lock().unwrap().clone());
            let n = 120usize;
            let bw = wr.width() / n as f32;
            for i in 0..n {
                let v = match &wave {
                    Some(w) => {
                        let a = i * w.len() / n;
                        let b = ((i + 1) * w.len() / n).max(a + 1).min(w.len());
                        w[a..b].iter().cloned().fold(0.0, f32::max)
                    }
                    None => 0.06,
                };
                let hgt = (v * wr.height()).max(3.0);
                let x = wr.left() + i as f32 * bw + bw / 2.0;
                let played = (i as f32 + 0.5) / n as f32 <= frac;
                let col = if played { TEXT } else { Color32::from_white_alpha(46) };
                painter.line_segment([pos2(x, wr.center().y - hgt / 2.0), pos2(x, wr.center().y + hgt / 2.0)], Stroke::new((bw * 0.55).max(1.5), col));
            }
            let resp = ui.interact(wr, Id::new("wave"), Sense::click());
            if resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            }
            if resp.clicked() && dur > 0.0 {
                if let Some(p) = resp.interact_pointer_pos() {
                    acts.push(Act::Seek(((p.x - wr.left()) / wr.width()).clamp(0.0, 1.0) as f64 * dur));
                }
            }
            if self.player.as_ref().map(|p| !p.paused).unwrap_or(false) {
                ui.ctx().request_repaint_after_secs(0.05);
            }
            y = wr.bottom() + SP_40;
        }
        painter.text(pos2(area.center().x, y), Align2::CENTER_TOP, title, medium(T_TITLE), TEXT);
        if !sub.is_empty() {
            painter.text(pos2(area.center().x, y + 32.0), Align2::CENTER_TOP, sub.join(" · "), light(T_BODY), TEXT_SECONDARY);
        }
    }

    fn draw_text(&mut self, ui: &mut Ui, area: Rect, path: &Path, text: &str, truncated: bool) {
        if self.text_lines.as_ref().map(|(p, _)| p != path).unwrap_or(true) {
            let lines: Vec<String> = text.lines().map(|l| l.replace('\t', "    ")).collect();
            self.text_lines = Some((path.to_path_buf(), lines));
        }
        let inner = Rect::from_min_max(area.min + vec2(SP_24, SP_16), area.max - vec2(SP_8, 92.0));
        let lines = &self.text_lines.as_ref().unwrap().1;
        let row_h = 19.0;
        let gutter = format!("{}", lines.len()).len() as f32 * 7.6 + SP_16;
        ui.scope_builder(UiBuilder::new().max_rect(inner), |ui| {
            egui::ScrollArea::both().id_salt(path).auto_shrink([false, false]).show_rows(ui, row_h, lines.len() + usize::from(truncated), |ui, range| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for i in range {
                    ui.horizontal(|ui| {
                        ui.set_height(row_h);
                        ui.add_sized([gutter, row_h], egui::Label::new(RichText::new(format!("{}", i + 1)).font(mono(T_FOOTNOTE)).color(Color32::from_white_alpha(70))));
                        ui.add_space(SP_12);
                        if let Some(l) = lines.get(i) {
                            ui.add(egui::Label::new(RichText::new(l).font(mono(T_FOOTNOTE + 0.5)).color(TEXT)).extend());
                        } else {
                            ui.label(RichText::new(tr!("Показаны первые 4 МБ файла", "Showing the first 4 MB of the file", "Показано перші 4 МБ файлу")).font(regular(T_FOOTNOTE)).color(TEXT_SECONDARY));
                        }
                    });
                }
            });
        });
    }

    fn file_header(&self, ui: &mut Ui, center: Pos2, path: &Path) -> f32 {
        let painter = ui.painter();
        let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
        let g = painter.layout(name, medium(T_TITLE), TEXT, 520.0);
        let h = g.size().y;
        painter.galley(pos2(center.x - g.size().x / 2.0, center.y), g, TEXT);
        let size = std::fs::metadata(path).map(|m| fmt_bytes(m.len())).unwrap_or_default();
        let sub = format!("{} · {}", kind_label(kind_of(path), &ext_of(path)), size);
        painter.text(pos2(center.x, center.y + h + SP_8), Align2::CENTER_TOP, sub, light(T_BODY), TEXT_SECONDARY);
        h + SP_8 + 22.0
    }

    fn draw_preview(&mut self, ui: &mut Ui, area: Rect, path: &Path, image: Option<&Arc<ColorImage>>, acts: &mut Vec<Act>) {
        let ctx = ui.ctx().clone();
        let mut y = area.center().y - 140.0;
        if let Some(img) = image {
            let entry = self.tex.entry(path.to_path_buf()).or_insert_with(|| vec![None]);
            if entry[0].is_none() {
                entry[0] = Some(ctx.load_texture("preview", ImageData::Color(img.clone()), TEX_STATIC));
            }
            let t = entry[0].clone().unwrap();
            let s = 200.0 / (img.size[0].max(img.size[1]) as f32);
            let sz = vec2(img.size[0] as f32 * s, img.size[1] as f32 * s);
            let r = Rect::from_center_size(pos2(area.center().x, y + 100.0), sz);
            draw_tex(ui.painter(), t.id(), r, 0, Color32::WHITE);
            y += 200.0 + SP_24;
        }
        let h = self.file_header(ui, pos2(area.center().x, y), path);
        y += h + SP_24;
        if pill_button(ui, Id::new("openwith"), pos2(area.center().x, y + PILL_H / 2.0), tr!("Открыть с помощью…", "Open with…", "Відкрити за допомогою…"), true).clicked() {
            acts.push(Act::OpenWith);
        }
        if link(ui, Id::new("showfolder"), pos2(area.center().x, y + PILL_H + SP_16), Align2::CENTER_TOP, tr!("Показать в папке", "Show in folder", "Показати в теці")).clicked() {
            acts.push(Act::ShowFolder);
        }
    }

    fn draw_error(&mut self, ui: &mut Ui, area: Rect, path: &Path, err: &str, acts: &mut Vec<Act>) {
        let painter = ui.painter().clone();
        let mut y = area.center().y - 110.0;
        painter.text(pos2(area.center().x, y), Align2::CENTER_TOP, tr!("Не удалось открыть файл", "Could not open the file", "Не вдалося відкрити файл"), medium(T_TITLE), TEXT);
        y += 34.0;
        let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
        painter.text(pos2(area.center().x, y), Align2::CENTER_TOP, name, light(T_BODY), TEXT_SECONDARY);
        y += 28.0;
        let g = painter.layout(err.to_string(), regular(T_FOOTNOTE), TEXT_SECONDARY, 480.0_f32.min(area.width() - 48.0));
        let gh = g.size().y;
        painter.galley(pos2(area.center().x - g.size().x / 2.0, y), g, TEXT_SECONDARY);
        y += gh + SP_24;
        if pill_button(ui, Id::new("err_openwith"), pos2(area.center().x, y + PILL_H / 2.0), tr!("Открыть с помощью…", "Open with…", "Відкрити за допомогою…"), false).clicked() {
            acts.push(Act::OpenWith);
        }
        if link(ui, Id::new("err_folder"), pos2(area.center().x, y + PILL_H + SP_16), Align2::CENTER_TOP, tr!("Показать в папке", "Show in folder", "Показати в теці")).clicked() {
            acts.push(Act::ShowFolder);
        }
    }

    fn draw_empty(&mut self, ui: &mut Ui, area: Rect, acts: &mut Vec<Act>) {
        let painter = ui.painter().clone();
        let c = area.center();
        let frame = Rect::from_center_size(c - vec2(0.0, 96.0), vec2(120.0, 96.0));
        widgets::viewfinder(&painter, frame, 22.0, Stroke::new(2.0, TEXT));
        painter.circle_filled(frame.center(), 9.0, ACCENT);
        painter.text(c + vec2(0.0, -8.0), Align2::CENTER_TOP, tr!("Откройте фото или видео", "Open a photo or video", "Відкрийте фото або відео"), medium(T_TITLE), TEXT);
        painter.text(c + vec2(0.0, 26.0), Align2::CENTER_TOP, tr!("Перетащите файл в окно или выберите его на компьютере", "Drag a file into the window or choose one on your computer", "Перетягніть файл у вікно або виберіть його на комп'ютері"), light(T_BODY), TEXT_SECONDARY);
        if pill_button(ui, Id::new("choose"), c + vec2(0.0, 92.0), tr!("Выбрать файл", "Choose file", "Вибрати файл"), true).clicked() {
            acts.push(Act::Open);
        }
        if !register::is_registered() {
            if link(ui, Id::new("mkdefault"), c + vec2(0.0, 132.0), Align2::CENTER_TOP, tr!("Открывать фото и видео в JustView", "Open photos and videos in JustView", "Відкривати фото й відео в JustView")).clicked() {
                acts.push(Act::MakeDefault);
            }
        } else if link(ui, Id::new("defaults"), c + vec2(0.0, 132.0), Align2::CENTER_TOP, tr!("Приложения по умолчанию", "Default apps", "Програми за замовчуванням")).clicked() {
            register::open_default_apps();
        }
        painter.text(pos2(c.x, area.bottom() - SP_24), Align2::CENTER_BOTTOM, trf!(["JustView {VERSION} · F1 - сочетания клавиш", "JustView {VERSION} · F1 - keyboard shortcuts", "JustView {VERSION} · F1 - комбінації клавіш"]), regular(T_CAPTION), TEXT_SECONDARY);
    }

    // ------------------------------------------------------------------

    fn info_rows(&self, path: &Path, d: Option<&Decoded>) -> Vec<(String, String)> {
        let mut rows = Vec::new();
        rows.push((tr!("Тип", "Type", "Тип").to_string(), kind_label(kind_of(path), &ext_of(path))));
        if let Ok(m) = std::fs::metadata(path) {
            rows.push((tr!("Размер файла", "File size", "Розмір файлу").into(), fmt_bytes(m.len())));
            if let Ok(t) = m.modified() {
                rows.push((tr!("Изменён", "Modified", "Змінено").into(), winutil::fmt_local(t)));
            }
        }
        if let Some(d) = d {
            match &d.content {
                Content::Image { orig, frames, .. } => {
                    rows.push((tr!("Разрешение", "Resolution", "Роздільність").into(), format!("{} × {}", orig[0], orig[1])));
                    let mp = orig[0] as f64 * orig[1] as f64 / 1e6;
                    if mp >= 0.1 {
                        rows.push((tr!("Мегапиксели", "Megapixels", "Мегапікселі").into(), trf!(["{mp:.1} Мп", "{mp:.1} MP", "{mp:.1} Мп"]).replace('.', ",")));
                    }
                    if frames.len() > 1 {
                        let total: f32 = frames.iter().map(|f| f.delay).sum();
                        rows.push((tr!("Анимация", "Animation", "Анімація").into(), trf!(["{} кадров · {}", "{} frames · {}", "{} кадрів · {}"], frames.len(), fmt_time(total as f64))));
                    }
                }
                Content::Media { info, .. } => {
                    if info.duration > 0.0 {
                        rows.push((tr!("Длительность", "Duration", "Тривалість").into(), fmt_time(info.duration)));
                    }
                    if info.has_video {
                        rows.push((tr!("Видео", "Video", "Відео").into(), trf!(["{} × {} · {:.2} к/с · {}", "{} × {} · {:.2} fps · {}", "{} × {} · {:.2} к/с · {}"], info.width, info.height, info.fps, info.video_codec.to_uppercase()).replace(".00 ", " ")));
                    }
                    if info.has_audio {
                        rows.push((tr!("Аудио", "Audio", "Аудіо").into(), trf!(["{} · {} Гц · {} кан", "{} · {} Hz · {} ch", "{} · {} Гц · {} кан"], info.audio_codec.to_uppercase(), info.sample_rate, info.channels)));
                        if info.audio_tracks.len() > 1 {
                            let names: Vec<String> = info.audio_tracks.iter().map(|t| t.label()).collect();
                            rows.push((tr!("Дорожки", "Tracks", "Доріжки").into(), names.join(", ")));
                        }
                    }
                    if info.bitrate > 0 {
                        rows.push((tr!("Битрейт", "Bitrate", "Бітрейт").into(), trf!(["{} кбит/с", "{} kbps", "{} кбіт/с"], info.bitrate / 1000)));
                    }
                    if !info.format.is_empty() {
                        rows.push((tr!("Контейнер", "Container", "Контейнер").into(), info.format.clone()));
                    }
                    for (k, v) in [(tr!("Название", "Title", "Назва"), &info.title), (tr!("Исполнитель", "Artist", "Виконавець"), &info.artist), (tr!("Альбом", "Album", "Альбом"), &info.album)] {
                        if !v.is_empty() {
                            rows.push((k.into(), v.clone()));
                        }
                    }
                }
                Content::Pdf { pages, .. } => rows.push((tr!("Страниц", "Pages", "Сторінок").into(), pages.to_string())),
                Content::Text { text, .. } => rows.push((tr!("Строк", "Lines", "Рядків").into(), text.lines().count().to_string())),
                _ => {}
            }
            rows.extend(d.exif.iter().cloned());
        }
        rows
    }

    fn info_panel(&mut self, ui: &mut Ui, panel: Rect, path: Option<&Path>, d: Option<&Decoded>, acts: &mut Vec<Act>) {
        let painter = ui.painter().clone();
        self.chrome_rects.push(panel);
        ui.interact(panel, Id::new("info_bg"), Sense::click());
        painter.rect_filled(panel, 0, SURFACE);
        let close = Rect::from_min_size(pos2(panel.right() - SP_12 - ICON_BTN, panel.top() + SP_12), Vec2::splat(ICON_BTN));
        painter.text(pos2(panel.left() + SP_24, close.center().y), Align2::LEFT_CENTER, tr!("Сведения", "Info", "Відомості"), medium(T_HEADLINE), TEXT);
        if icon_button(ui, Id::new("info_close"), close, Icon::Close, tr!("Закрыть  I", "Close  I", "Закрити  I"), true, 1.0).clicked() {
            acts.push(Act::Info);
        }
        let Some(path) = path else { return };
        let rows = self.info_rows(path, d);
        let body = Rect::from_min_max(pos2(panel.left(), close.bottom() + SP_12), panel.max);
        ui.scope_builder(UiBuilder::new().max_rect(body), |ui| {
            egui::ScrollArea::vertical().id_salt("info_scroll").auto_shrink([false, false]).show(ui, |ui| {
                ui.add_space(SP_4);
                let left = SP_24;
                let width = INFO_W - SP_24 * 2.0;
                ui.horizontal(|ui| {
                    ui.add_space(left);
                    ui.allocate_ui_with_layout(vec2(width, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                        ui.set_width(width);
                        ui.add(egui::Label::new(RichText::new(path.file_name().unwrap_or_default().to_string_lossy()).font(medium(T_BODY)).color(TEXT)).wrap());
                    });
                });
                ui.add_space(SP_16);
                for (k, v) in rows {
                    let top = ui.cursor().top();
                    ui.horizontal(|ui| {
                        ui.add_space(left);
                        ui.allocate_ui_with_layout(vec2(width, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                            ui.set_width(width);
                            ui.spacing_mut().item_spacing.y = 2.0;
                            ui.add_space(SP_8);
                            ui.label(RichText::new(k).font(regular(T_CAPTION)).color(TEXT_SECONDARY));
                            ui.add(egui::Label::new(RichText::new(v).font(regular(T_SUBHEAD)).color(TEXT)).wrap());
                            ui.add_space(SP_8);
                        });
                    });
                    let _ = top;
                    let y = ui.cursor().top();
                    ui.painter().line_segment([pos2(panel.left() + left, y), pos2(panel.right() - SP_24, y)], Stroke::new(1.0, HAIRLINE));
                }
                ui.add_space(SP_16);
                ui.horizontal(|ui| {
                    ui.add_space(left);
                    let r = ui.add(egui::Label::new(RichText::new(tr!("Показать в папке ›", "Show in folder ›", "Показати в теці ›")).font(regular(T_SUBHEAD)).color(ACCENT)).sense(Sense::click()));
                    if r.hovered() {
                        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                    }
                    if r.clicked() {
                        acts.push(Act::ShowFolder);
                    }
                });
                ui.horizontal(|ui| {
                    ui.add_space(left);
                    let r = ui.add(egui::Label::new(RichText::new(tr!("Открыть с помощью… ›", "Open with… ›", "Відкрити за допомогою… ›")).font(regular(T_SUBHEAD)).color(ACCENT)).sense(Sense::click()));
                    if r.hovered() {
                        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                    }
                    if r.clicked() {
                        acts.push(Act::OpenWith);
                    }
                });
                ui.add_space(SP_24);
                ui.horizontal(|ui| {
                    ui.add_space(left);
                    ui.label(RichText::new(trf!(["JustView {VERSION} · F1 - сочетания клавиш", "JustView {VERSION} · F1 - keyboard shortcuts", "JustView {VERSION} · F1 - комбінації клавіш"])).font(regular(T_CAPTION)).color(TEXT_SECONDARY));
                });
                ui.add_space(SP_24);
            });
        });
    }

    fn draw_toast(&mut self, ui: &mut Ui, area: Rect, now: f64) {
        let Some(t) = &mut self.toast else { return };
        if t.at.is_nan() {
            t.at = now;
        }
        let age = (now - t.at) as f32;
        let life = 2.6;
        if age > life {
            self.toast = None;
            return;
        }
        let a = (age / M_SMALL).min(1.0).min((life - age) / M_SMALL).clamp(0.0, 1.0);
        let painter = ui.painter();
        let g = painter.layout_no_wrap(t.text.clone(), regular(T_SUBHEAD), TEXT);
        let r = Rect::from_center_size(pos2(area.center().x, area.top() + SP_24 + 18.0 - (1.0 - a) * 6.0), g.size() + vec2(SP_40, 18.0));
        painter.rect_filled(r, (r.height() / 2.0) as u8, GLASS.gamma_multiply(a));
        painter.galley(r.center() - g.size() / 2.0, g, TEXT.gamma_multiply(a));
        ui.ctx().request_repaint();
    }

    fn draw_dialog(&mut self, ui: &mut Ui, full: Rect, acts: &mut Vec<Act>) {
        let ctx = ui.ctx().clone();
        let open = !matches!(self.dialog, Dialog::None);
        let t = ease(ctx.animate_bool_with_time(Id::new("dialog"), open, if reduced_motion() { 0.0 } else { M_SMALL }));
        if t <= 0.0 {
            return;
        }
        let painter = ui.painter().clone();
        painter.rect_filled(full, 0, Color32::from_black_alpha((150.0 * t) as u8));
        let scrim = ui.interact(full, Id::new("scrim"), Sense::click());
        let (esc, enter, del) = ctx.input(|i| (i.key_pressed(Key::Escape), i.key_pressed(Key::Enter), i.key_pressed(Key::Delete)));
        match &self.dialog {
            Dialog::Delete(p) => {
                let p = p.clone();
                let w = 400.0;
                let h = 188.0;
                let card = Rect::from_center_size(full.center() + vec2(0.0, (1.0 - t) * 12.0), vec2(w, h));
                painter.rect_filled(card, R_CARD, SURFACE.gamma_multiply(t));
                ui.interact(card, Id::new("dlg_card"), Sense::click());
                painter.text(card.min + vec2(SP_24, SP_24), Align2::LEFT_TOP, tr!("Удалить файл?", "Delete file?", "Видалити файл?"), medium(T_HEADLINE), TEXT.gamma_multiply(t));
                let name = p.file_name().unwrap_or_default().to_string_lossy().to_string();
                let g = painter.layout(trf!(["«{name}» будет перемещён в Корзину - его можно будет восстановить", "“{name}” will be moved to the Recycle Bin - you can restore it", "«{name}» буде переміщено до Кошика - його можна буде відновити"]), light(T_BODY), TEXT_SECONDARY.gamma_multiply(t), w - SP_48);
                painter.galley(card.min + vec2(SP_24, SP_24 + 32.0), g, TEXT_SECONDARY);
                let by = card.bottom() - SP_24 - PILL_H / 2.0;
                let ok = pill_button(ui, Id::new("dlg_ok"), pos2(card.right() - SP_24 - 66.0, by), tr!("В корзину", "Move to Bin", "До кошика"), true);
                let cancel = pill_button(ui, Id::new("dlg_cancel"), pos2(card.right() - SP_24 - 132.0 - SP_12 - 50.0, by), tr!("Отмена", "Cancel", "Скасувати"), false);
                if ok.clicked() || enter || del {
                    self.dialog = Dialog::None;
                    self.delete_confirmed(p);
                } else if cancel.clicked() || esc || scrim.clicked() {
                    self.dialog = Dialog::None;
                    self.player_path = None; // перезапустить плеер
                }
            }
            Dialog::Help => {
                let rows: &[(&str, &str)] = &[
                    ("← →", tr!("Предыдущий / следующий файл", "Previous / next file", "Попередній / наступний файл")),
                    (tr!("Колесо, + −", "Wheel, + −", "Коліщатко, + −"), tr!("Масштаб у курсора", "Zoom at cursor", "Масштаб біля курсора")),
                    (tr!("0 или двойной щелчок", "0 or double-click", "0 або подвійне клацання"), tr!("Вписать / 100%", "Fit / 100%", "Вписати / 100%")),
                    ("R / Shift+R", tr!("Повернуть", "Rotate", "Повернути")),
                    ("F, F11", tr!("Во весь экран", "Full screen", "На весь екран")),
                    ("I", tr!("Сведения о файле", "File info", "Відомості про файл")),
                    ("Del", tr!("В Корзину", "To Recycle Bin", "До Кошика")),
                    ("Ctrl+C / Ctrl+Shift+C", tr!("Копировать изображение / путь", "Copy image / path", "Копіювати зображення / шлях")),
                    ("Ctrl+O, Ctrl+E", tr!("Открыть / показать в папке", "Open / show in folder", "Відкрити / показати в теці")),
                    (tr!("Пробел, K", "Space, K", "Пробіл, K"), tr!("Пауза (видео, аудио, GIF)", "Pause (video, audio, GIF)", "Пауза (відео, аудіо, GIF)")),
                    ("← → , J L", tr!("Перемотка 5 с / 10 с", "Seek 5 s / 10 s", "Перемотування 5 с / 10 с")),
                    ("Shift+← →", tr!("Файлы при просмотре видео", "Files while watching video", "Файли під час перегляду відео")),
                    (", .", tr!("Кадр назад / вперёд", "Frame back / forward", "Кадр назад / уперед")),
                    ("[ ]  Backspace", tr!("Скорость 0,25-3× / сброс", "Speed 0.25-3× / reset", "Швидкість 0,25-3× / скидання")),
                    ("A", tr!("Следующая звуковая дорожка", "Next audio track", "Наступна звукова доріжка")),
                    ("↑ ↓, M", tr!("Громкость, без звука", "Volume, mute", "Гучність, без звуку")),
                    ("Esc", tr!("Выйти из полноэкранного / закрыть", "Exit full screen / close", "Вийти з повноекранного / закрити")),
                ];
                let w = 560.0_f32.min(full.width() - 32.0);
                let row_h = 26.0;
                let h = SP_24 * 2.0 + 36.0 + rows.len() as f32 * row_h + SP_16 + PILL_H;
                let card = Rect::from_center_size(full.center() + vec2(0.0, (1.0 - t) * 12.0), vec2(w, h));
                painter.rect_filled(card, R_CARD, SURFACE.gamma_multiply(t));
                ui.interact(card, Id::new("help_card"), Sense::click());
                painter.text(card.min + vec2(SP_24, SP_24), Align2::LEFT_TOP, tr!("Сочетания клавиш", "Keyboard shortcuts", "Комбінації клавіш"), medium(T_HEADLINE), TEXT.gamma_multiply(t));
                for (i, (k, v)) in rows.iter().enumerate() {
                    let y = card.top() + SP_24 + 36.0 + i as f32 * row_h + row_h / 2.0;
                    painter.text(pos2(card.left() + SP_24, y), Align2::LEFT_CENTER, *k, regular(T_FOOTNOTE), TEXT.gamma_multiply(t));
                    painter.text(pos2(card.left() + SP_24 + 200.0, y), Align2::LEFT_CENTER, *v, light(T_SUBHEAD), TEXT_SECONDARY.gamma_multiply(t));
                }
                let lang_label = format!("{}: {}", tr!("Язык", "Language", "Мова"), crate::i18n::lang().native_name());
                let lg = painter.layout_no_wrap(lang_label.clone(), regular(T_SUBHEAD + 1.0), TEXT);
                let lc = pos2(card.left() + SP_24 + (lg.size().x + 44.0) / 2.0, card.bottom() - SP_24 - PILL_H / 2.0);
                let to_lang = pill_button(ui, Id::new("help_lang"), lc, &lang_label, false).clicked();
                let close = Rect::from_min_size(pos2(card.right() - SP_12 - ICON_BTN, card.top() + SP_12), Vec2::splat(ICON_BTN));
                if to_lang {
                    self.dialog = Dialog::Language(false);
                } else if icon_button(ui, Id::new("help_close"), close, Icon::Close, "", true, t).clicked() || esc || scrim.clicked() {
                    self.dialog = Dialog::None;
                }
            }
            Dialog::Language(first) => {
                let first = *first;
                let row_h = 52.0;
                let w = 360.0_f32.min(full.width() - 32.0);
                let h = SP_24 * 2.0 + 36.0 + 3.0 * row_h + SP_8;
                let card = Rect::from_center_size(full.center() + vec2(0.0, (1.0 - t) * 12.0), vec2(w, h));
                painter.rect_filled(card, R_CARD, SURFACE.gamma_multiply(t));
                ui.interact(card, Id::new("lang_card"), Sense::click());
                // На первом запуске заголовок понятен на любом из трёх языков.
                let title = if first { "Язык · Language · Мова" } else { tr!("Язык", "Language", "Мова") };
                painter.text(card.min + vec2(SP_24, SP_24), Align2::LEFT_TOP, title, medium(T_HEADLINE), TEXT.gamma_multiply(t));
                let cur = crate::i18n::lang();
                let mut chosen = None;
                for (i, l) in crate::i18n::ALL.iter().enumerate() {
                    let r = Rect::from_min_size(pos2(card.left() + SP_12, card.top() + SP_24 + 36.0 + i as f32 * row_h), vec2(w - SP_24, row_h - 4.0));
                    let id = Id::new(("lang_row", i));
                    let rr = ui.interact(r, id, Sense::click());
                    let hv = ctx.animate_bool_with_time(id.with("h"), rr.hovered(), M_MICRO);
                    let sel = *l == cur;
                    painter.rect_filled(r, R_FIELD, SURFACE_RAISED.gamma_multiply(t * (if sel { 1.0 } else { 0.35 + 0.65 * hv })));
                    painter.text(pos2(r.left() + SP_16, r.center().y), Align2::LEFT_CENTER, l.native_name(), regular(T_BODY), if sel { ACCENT } else { TEXT }.gamma_multiply(t));
                    if sel {
                        let c = pos2(r.right() - SP_24, r.center().y);
                        painter.add(Shape::line(vec![c + vec2(-5.0, 0.0), c + vec2(-1.5, 4.0), c + vec2(6.0, -5.0)], Stroke::new(1.8, ACCENT.gamma_multiply(t))));
                    }
                    if rr.hovered() {
                        ctx.set_cursor_icon(CursorIcon::PointingHand);
                    }
                    if rr.clicked() {
                        chosen = Some(*l);
                    }
                }
                let digit = ctx.input(|i| [Key::Num1, Key::Num2, Key::Num3].iter().position(|k| i.key_pressed(*k)));
                if let Some(d) = digit {
                    chosen = Some(crate::i18n::ALL[d]);
                }
                if let Some(l) = chosen {
                    crate::i18n::set(l);
                    self.settings.lang = Some(l.code().to_string());
                    self.settings.save();
                    self.dialog = Dialog::None;
                } else if (esc || scrim.clicked() || enter) && !first {
                    self.dialog = Dialog::None;
                } else if enter && first {
                    self.settings.lang = Some(cur.code().to_string());
                    self.settings.save();
                    self.dialog = Dialog::None;
                }
            }
            Dialog::None => {}
        }
        let _ = acts;
    }

    /// Карточка «Доступна новая версия» сверху по центру.
    fn draw_update(&mut self, ui: &mut Ui, area: Rect) {
        if self.update_hidden || !matches!(self.dialog, Dialog::None) {
            return;
        }
        let Some(rel) = self.update.as_ref().and_then(|s| s.lock().unwrap().clone()) else { return };
        let ctx = ui.ctx().clone();
        let t = ease(ctx.animate_bool_with_time(Id::new("update_card"), true, if reduced_motion() { 0.0 } else { M_SMALL }));
        let painter = ui.painter().clone();
        let title = trf!(["Доступна JustView {}", "JustView {} is available", "Доступна JustView {}"], rel.version);
        let g = painter.layout_no_wrap(title, medium(T_SUBHEAD), TEXT);
        let upd = tr!("Обновить", "Update", "Оновити");
        let later = tr!("Позже", "Later", "Пізніше");
        let skip = tr!("Пропустить", "Skip", "Пропустити");
        let bw = |s: &str| painter.layout_no_wrap(s.to_string(), regular(T_SUBHEAD + 1.0), TEXT).size().x + 44.0;
        let buttons = bw(upd) + bw(later) + bw(skip) + SP_8 * 2.0;
        let w = (g.size().x.max(buttons) + SP_24 * 2.0).min(area.width() - SP_32);
        let h = SP_16 * 2.0 + 22.0 + SP_12 + PILL_H;
        let card = Rect::from_min_size(pos2(area.center().x - w / 2.0, area.top() + SP_20 - (1.0 - t) * 8.0), vec2(w, h));
        self.chrome_rects.push(card);
        ui.interact(card, Id::new("update_bg"), Sense::click());
        painter.rect_filled(card, R_CARD, GLASS.gamma_multiply(t));
        painter.galley(card.min + vec2(SP_24, SP_16), g, TEXT.gamma_multiply(t));
        let y = card.bottom() - SP_16 - PILL_H / 2.0;
        let mut x = card.left() + SP_24;
        let mut button = |label: &str, primary: bool, id: &str| {
            let wb = bw(label);
            let r = pill_button(ui, Id::new(id), pos2(x + wb / 2.0, y), label, primary);
            x += wb + SP_8;
            r.clicked()
        };
        let do_update = button(upd, true, "upd_go");
        let do_later = button(later, false, "upd_later");
        let do_skip = button(skip, false, "upd_skip");
        if do_update {
            let args: Vec<std::ffi::OsString> = self.cur().map(|p| vec![p.clone().into_os_string()]).unwrap_or_default();
            self.toast(tr!("Загрузка обновления…", "Downloading the update…", "Завантаження оновлення…"));
            self.update_hidden = true;
            std::thread::spawn(move || match crate::update::install(&rel, UA, &args) {
                Ok(()) => std::process::exit(0),
                Err(_) => crate::update::open_page(&rel.page),
            });
        } else if do_later {
            self.update_hidden = true;
        } else if do_skip {
            self.settings.skip_version = rel.version.clone();
            self.settings.save();
            self.update_hidden = true;
        }
    }
}

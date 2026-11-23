//! Пул фоновых потоков: декодирование, страницы PDF, чтение папки.

use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex, OnceLock};

use egui::ColorImage;

use crate::decode::{self, Decoded};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Job {
    Decode(PathBuf),
    PdfPage(PathBuf, u32, u32),
    ScanDir(PathBuf, PathBuf),
}

pub enum Done {
    Decoded(PathBuf, Arc<Decoded>),
    PdfPage(PathBuf, u32, Result<Arc<ColorImage>, String>),
    Dir(PathBuf, Vec<PathBuf>),
}

struct Queue {
    jobs: VecDeque<Job>,
    in_flight: HashSet<Job>,
}

pub struct Loader {
    q: Arc<(Mutex<Queue>, Condvar)>,
    rx: Receiver<Done>,
    pub max_side: Arc<AtomicU32>,
    ctx: Arc<OnceLock<egui::Context>>,
}

impl Loader {
    pub fn new(threads: usize) -> Self {
        let q = Arc::new((
            Mutex::new(Queue { jobs: VecDeque::new(), in_flight: HashSet::new() }),
            Condvar::new(),
        ));
        let (tx, rx) = channel();
        let max_side = Arc::new(AtomicU32::new(8192));
        let ctx: Arc<OnceLock<egui::Context>> = Arc::new(OnceLock::new());
        for i in 0..threads {
            let q = q.clone();
            let tx: Sender<Done> = tx.clone();
            let max_side = max_side.clone();
            let ctx = ctx.clone();
            std::thread::Builder::new()
                .name(format!("jv-loader-{i}"))
                .spawn(move || worker(q, tx, max_side, ctx))
                .unwrap();
        }
        Self { q, rx, max_side, ctx }
    }

    pub fn set_ctx(&self, ctx: &egui::Context) {
        let _ = self.ctx.set(ctx.clone());
    }

    /// `urgent` — в начало очереди.
    pub fn request(&self, job: Job, urgent: bool) {
        let (m, cv) = &*self.q;
        let mut q = m.lock().unwrap();
        if q.in_flight.contains(&job) {
            return;
        }
        if let Some(pos) = q.jobs.iter().position(|j| *j == job) {
            if urgent && pos != 0 {
                let j = q.jobs.remove(pos).unwrap();
                q.jobs.push_front(j);
            }
            return;
        }
        if urgent {
            q.jobs.push_front(job);
        } else {
            q.jobs.push_back(job);
        }
        cv.notify_one();
    }

    /// Убрать из очереди задачи декодирования, которые больше не нужны.
    pub fn retain_decodes(&self, wanted: &[PathBuf]) {
        let (m, _) = &*self.q;
        let mut q = m.lock().unwrap();
        q.jobs.retain(|j| match j {
            Job::Decode(p) => wanted.iter().any(|w| w == p),
            _ => true,
        });
    }

    pub fn poll(&self) -> Vec<Done> {
        self.rx.try_iter().collect()
    }
}

fn worker(
    q: Arc<(Mutex<Queue>, Condvar)>,
    tx: Sender<Done>,
    max_side: Arc<AtomicU32>,
    ctx: Arc<OnceLock<egui::Context>>,
) {
    loop {
        let job = {
            let (m, cv) = &*q;
            let mut g = m.lock().unwrap();
            loop {
                if let Some(j) = g.jobs.pop_front() {
                    g.in_flight.insert(j.clone());
                    break j;
                }
                g = cv.wait(g).unwrap();
            }
        };
        let done = match &job {
            Job::Decode(p) => {
                let d = decode::decode(p, max_side.load(Ordering::Relaxed));
                Done::Decoded(p.clone(), Arc::new(d))
            }
            Job::PdfPage(p, page, width) => {
                let r = crate::pdf::render_page(p, *page, *width).map(|(_, r)| {
                    Arc::new(ColorImage::from_rgba_unmultiplied([r.w as usize, r.h as usize], &r.px))
                });
                Done::PdfPage(p.clone(), *page, r)
            }
            Job::ScanDir(dir, opened) => Done::Dir(dir.clone(), scan_dir(dir, opened)),
        };
        {
            let (m, _) = &*q;
            m.lock().unwrap().in_flight.remove(&job);
        }
        if tx.send(done).is_err() {
            return;
        }
        if let Some(c) = ctx.get() {
            c.request_repaint();
        }
    }
}

pub fn scan_dir(dir: &Path, opened: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
            .map(|e| e.path())
            .filter(|p| p == opened || crate::formats::is_browsable(p, opened))
            .collect(),
        Err(_) => vec![opened.to_path_buf()],
    };
    v.sort_by(|a, b| {
        crate::winutil::natural_cmp(
            a.file_name().unwrap_or_default(),
            b.file_name().unwrap_or_default(),
        )
    });
    v
}

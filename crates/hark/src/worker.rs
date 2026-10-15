//! Background threads: decoding, peaks, loudness, exports and scans, with results polled by
//! the UI once per frame.

use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};

use hark_core::config::Root;
use hark_core::export::{ExportRequest, ExportResult};
use hark_core::library::ScanReport;
use hark_core::{Loudness, Pcm, Peaks};

pub struct Loaded {
    pub path: PathBuf,
    pub source_rate: u32,
    pub channels: u16,
    pub duration_secs: f64,
    /// At the device rate, ready to play.
    pub device: Arc<Pcm>,
    pub peaks: Arc<Peaks>,
    pub loudness: Option<Loudness>,
}

pub enum Job {
    Load { path: PathBuf, device_rate: u32 },
    Export(ExportRequest),
    Scan(Vec<Root>),
}

pub enum Done {
    Loaded(PathBuf, Result<Arc<Loaded>, String>),
    Exported(Result<ExportResult, String>),
    Scanned(Vec<(String, ScanReport)>),
}

pub struct Worker {
    tx: Sender<Job>,
    rx: Receiver<Done>,
}

impl Worker {
    pub fn start(ctx: egui::Context, threads: usize) -> Worker {
        let (tx, job_rx) = channel::<Job>();
        let (done_tx, rx) = channel::<Done>();
        let job_rx = Arc::new(Mutex::new(job_rx));
        for _ in 0..threads {
            let job_rx = job_rx.clone();
            let done_tx = done_tx.clone();
            let ctx = ctx.clone();
            std::thread::spawn(move || loop {
                let job = match job_rx.lock().unwrap().recv() {
                    Ok(j) => j,
                    Err(_) => return,
                };
                let done = match job {
                    Job::Load { path, device_rate } => Done::Loaded(path.clone(), load(&path, device_rate).map(Arc::new).map_err(|e| format!("{e:#}"))),
                    Job::Export(req) => Done::Exported(hark_core::export::run(&req).map_err(|e| format!("{e:#}"))),
                    Job::Scan(roots) => Done::Scanned(scan(&roots)),
                };
                if done_tx.send(done).is_err() {
                    return;
                }
                ctx.request_repaint();
            });
        }
        Worker { tx, rx }
    }

    pub fn send(&self, job: Job) {
        let _ = self.tx.send(job);
    }

    pub fn poll(&self) -> Vec<Done> {
        self.rx.try_iter().collect()
    }
}

fn peaks_cache_path(path: &std::path::Path) -> Option<PathBuf> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_nanos();
    let key = blake3::hash(format!("{}|{}|{}", path.display(), meta.len(), mtime).as_bytes()).to_hex();
    Some(hark_core::paths::peaks_cache_dir().join(format!("{key}.peaks")))
}

fn load(path: &std::path::Path, device_rate: u32) -> anyhow::Result<Loaded> {
    let source = hark_core::decode(path)?;
    let cache = peaks_cache_path(path);
    let peaks = match cache.as_ref().and_then(|c| Peaks::load(c).ok()) {
        Some(p) => p,
        None => {
            let p = Peaks::from_pcm(&source, Peaks::DEFAULT_BIN);
            if let Some(c) = &cache {
                if hark_core::paths::ensure_parent(c).is_ok() {
                    let _ = p.save(c);
                }
            }
            p
        }
    };
    let loudness = hark_core::loudness::measure(&source).ok();
    let device = if source.sample_rate == device_rate { source.clone() } else { hark_core::resample::resample(&source, device_rate)? };
    Ok(Loaded {
        path: path.to_path_buf(),
        source_rate: source.sample_rate,
        channels: source.channels,
        duration_secs: source.duration_secs(),
        device: Arc::new(device),
        peaks: Arc::new(peaks),
        loudness,
    })
}

fn scan(roots: &[Root]) -> Vec<(String, ScanReport)> {
    let mut out = Vec::new();
    let Ok(mut db) = hark_core::Db::open() else { return out };
    for r in roots {
        match db.scan(&r.path, |_, _| {}) {
            Ok(report) => out.push((r.name.clone(), report)),
            Err(e) => out.push((r.name.clone(), ScanReport { errors: vec![(r.path.clone(), format!("{e:#}"))], ..Default::default() })),
        }
    }
    out
}

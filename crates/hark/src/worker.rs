//! Background threads: decoding, peaks, loudness, exports and scans, with results polled by
//! the UI once per frame.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};

use hark_core::audition::Source;
use hark_core::config::Root;
use hark_core::copies::Plan;
use hark_core::export::{ExportRequest, ExportResult};
use hark_core::library::ScanReport;
use hark_core::{Loudness, Peaks};

use crate::dnd::Ready;

pub struct Loaded {
    pub path: PathBuf,
    pub source_rate: u32,
    pub channels: u16,
    pub duration_secs: f64,
    pub source: Source,
    /// `None` until a streamed file's analysis pass has run.
    pub peaks: Option<Arc<Peaks>>,
    /// Measured by the analysis pass; `None` before it runs, and for a file whose loudness
    /// only the library holds.
    pub loudness: Option<Loudness>,
}

impl Loaded {
    /// Memory held for this file: decoded audio and peaks.
    pub fn resident_bytes(&self) -> usize {
        let audio = match &self.source {
            Source::Whole(p) => p.samples.len() * 4,
            Source::File { .. } => 0,
        };
        let peaks = self.peaks.as_ref().map(|p| p.bins.iter().map(|b| b.len() * 12).sum()).unwrap_or(0);
        audio + peaks
    }
}

pub enum Job {
    Load { path: PathBuf, device_rate: u32, stream_above_mb: f64 },
    Export(ExportRequest),
    /// Writes a planned copy; `ready` tells a drag in flight when the file is there.
    Copy { plan: Plan, ready: Option<Ready> },
    Scan(Vec<Root>),
}

/// Where a running scan is: walking the roots, or hashing files so moved marks find theirs.
#[derive(Clone, Copy, Debug)]
pub enum ScanPhase {
    Walking { seen: usize },
    Matching { hashed: usize },
}

pub enum Done {
    Scanning(ScanPhase),
    Loaded(PathBuf, Result<Arc<Loaded>, String>),
    /// The analysis pass of a streamed file.
    Analyzed(PathBuf, Result<(Arc<Peaks>, Option<Loudness>), String>),
    Exported(Result<ExportResult, String>),
    /// `dragged` when the copy was for a drag, which is handed over by itself; otherwise the
    /// clipboard still wants it.
    Copied { result: Result<ExportResult, String>, dragged: bool },
    /// One result per root, by name; a root that could not be scanned carries the reason.
    Scanned(Vec<(String, Result<ScanReport, String>)>),
}

/// The one analysis pass in flight: a newer request replaces a waiting one and makes a
/// running one abort, so arrowing through long files never queues passes.
#[derive(Default)]
struct Analysis {
    next: Mutex<Option<(PathBuf, Option<Arc<Peaks>>)>>,
    wake: Condvar,
}

pub struct Worker {
    tx: Sender<Job>,
    rx: Receiver<Done>,
    analysis: Arc<Analysis>,
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
                    Job::Load { path, device_rate, stream_above_mb } => Done::Loaded(path.clone(), load(&path, device_rate, stream_above_mb).map(Arc::new).map_err(|e| format!("{e:#}"))),
                    Job::Export(req) => Done::Exported(hark_core::export::run(&req).map_err(|e| format!("{e:#}"))),
                    Job::Copy { plan, ready } => {
                        let result = hark_core::copies::finish(&plan).map_err(|e| format!("{e:#}"));
                        if let Some(r) = &ready {
                            r.set(result.is_ok());
                        }
                        Done::Copied { result, dragged: ready.is_some() }
                    }
                    Job::Scan(roots) => Done::Scanned(scan(&roots, |phase| {
                        let _ = done_tx.send(Done::Scanning(phase));
                        ctx.request_repaint();
                    })),
                };
                if done_tx.send(done).is_err() {
                    return;
                }
                ctx.request_repaint();
            });
        }
        let analysis = Arc::new(Analysis::default());
        {
            let analysis = analysis.clone();
            std::thread::spawn(move || loop {
                let (path, cached) = {
                    let mut next = analysis.next.lock().unwrap();
                    while next.is_none() {
                        next = analysis.wake.wait(next).unwrap();
                    }
                    next.take().unwrap()
                };
                let superseded = || analysis.next.lock().unwrap().is_some();
                let result = analyze(&path, cached, &superseded);
                if superseded() {
                    continue;
                }
                if done_tx.send(Done::Analyzed(path, result.map_err(|e| format!("{e:#}")))).is_err() {
                    return;
                }
                ctx.request_repaint();
            });
        }
        Worker { tx, rx, analysis }
    }

    pub fn send(&self, job: Job) {
        let _ = self.tx.send(job);
    }

    /// Asks for a streamed file's peaks (unless `cached`) and loudness, replacing any request
    /// still waiting.
    pub fn analyze(&self, path: PathBuf, cached: Option<Arc<Peaks>>) {
        *self.analysis.next.lock().unwrap() = Some((path, cached));
        self.analysis.wake.notify_one();
    }

    pub fn poll(&self) -> Vec<Done> {
        self.rx.try_iter().collect()
    }
}

fn peaks_cache_path(path: &Path) -> Option<PathBuf> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_nanos();
    let key = blake3::hash(format!("{}|{}|{}", path.display(), meta.len(), mtime).as_bytes()).to_hex();
    Some(hark_core::paths::peaks_cache_dir().join(format!("{key}.peaks")))
}

fn save_peaks(path: &Path, peaks: &Peaks) {
    if let Some(c) = peaks_cache_path(path) {
        if hark_core::paths::ensure_parent(&c).is_ok() {
            let _ = peaks.save(&c);
        }
    }
}

/// Decodes a file whole, or opens it for streaming when its decoded size is over the
/// threshold. Loudness is left to the analysis pass so playback never waits on it.
fn load(path: &Path, device_rate: u32, stream_above_mb: f64) -> anyhow::Result<Loaded> {
    let info = hark_core::probe(path)?;
    if info.decoded_mb() > stream_above_mb {
        let peaks = peaks_cache_path(path).and_then(|c| Peaks::load(&c).ok()).map(Arc::new);
        return Ok(Loaded {
            path: path.to_path_buf(),
            source_rate: info.sample_rate,
            channels: info.channels,
            duration_secs: info.duration_secs,
            source: Source::File {
                path: path.to_path_buf(),
                source_rate: info.sample_rate,
                channels: info.channels,
                source_frames: (info.duration_secs * info.sample_rate as f64).round() as u64,
            },
            peaks,
            loudness: None,
        });
    }
    let source = hark_core::decode(path)?;
    let peaks = match peaks_cache_path(path).and_then(|c| Peaks::load(&c).ok()) {
        Some(p) => p,
        None => {
            let p = Peaks::from_pcm(&source, Peaks::DEFAULT_BIN);
            save_peaks(path, &p);
            p
        }
    };
    let device = if source.sample_rate == device_rate { source.clone() } else { hark_core::resample::resample(&source, device_rate)? };
    Ok(Loaded {
        path: path.to_path_buf(),
        source_rate: source.sample_rate,
        channels: source.channels,
        duration_secs: source.duration_secs(),
        source: Source::Whole(Arc::new(device)),
        peaks: Some(Arc::new(peaks)),
        loudness: None,
    })
}

/// One pass over a streamed file for its peaks (unless cached) and loudness, holding only a
/// packet at a time; gives up as soon as `abort` says so.
fn analyze(path: &Path, cached: Option<Arc<Peaks>>, abort: &dyn Fn() -> bool) -> anyhow::Result<(Arc<Peaks>, Option<Loudness>)> {
    let mut reader = hark_core::decode::Reader::open(path)?;
    let mut chunk = Vec::new();
    let mut builder = None;
    let mut meter = None;
    while reader.next_chunk(&mut chunk)? {
        if cached.is_none() {
            builder.get_or_insert_with(|| hark_core::peaks::PeaksBuilder::new(reader.sample_rate, reader.channels, Peaks::DEFAULT_BIN)).push(&chunk);
        }
        if meter.is_none() {
            meter = hark_core::loudness::Meter::new(reader.channels, reader.sample_rate).ok();
        }
        if let Some(m) = &mut meter {
            m.push(&chunk)?;
        }
        chunk.clear();
        if abort() {
            anyhow::bail!("superseded");
        }
    }
    let peaks = match (cached, builder) {
        (Some(p), _) => p,
        (None, Some(b)) => {
            let p = b.finish();
            save_peaks(path, &p);
            Arc::new(p)
        }
        (None, None) => anyhow::bail!("no audio frames decoded"),
    };
    Ok((peaks, meter.and_then(|m| m.finish())))
}

/// Scans every root, then matches orphaned marks; `progress` hears the phase a few times a
/// second at most, so a fast walk does not flood the UI.
fn scan(roots: &[Root], mut progress: impl FnMut(ScanPhase)) -> Vec<(String, Result<ScanReport, String>)> {
    let Ok(mut db) = hark_core::Db::open() else { return Vec::new() };
    let mut last = std::time::Instant::now();
    let mut throttled = |phase: ScanPhase| {
        if last.elapsed() >= std::time::Duration::from_millis(100) {
            last = std::time::Instant::now();
            progress(phase);
        }
    };
    let mut seen = 0;
    let out = roots
        .iter()
        .map(|r| {
            let before = seen;
            let result = db.scan(&r.path, |_, n| {
                seen = before + n;
                throttled(ScanPhase::Walking { seen });
            });
            (r.name.clone(), result.map_err(|e| format!("{e:#}")))
        })
        .collect();
    let _ = db.match_orphaned_marks(|_, hashed| throttled(ScanPhase::Matching { hashed }));
    out
}

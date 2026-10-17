//! Background threads: decoding, peaks, loudness, exports and scans, with results polled by
//! the UI once per frame.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};

use hark_core::audition::Source;
use hark_core::config::Root;
use hark_core::config::Normalize;
use hark_core::copies::Plan;
use hark_core::export::{ExportRequest, ExportResult, PreviewLevel};
use hark_core::library::ScanReport;
use hark_core::pcm::DownmixMode;
use hark_core::{ExportPreset, Loudness, Pcm, Peaks};

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
    /// The preview made with a decoded file, so it never shows without it.
    pub prepared: Option<Prepared>,
}

/// A decoded file's preview under the preset it was loaded with.
#[derive(Clone)]
pub struct Prepared {
    pub mono: Option<DownmixMode>,
    pub normalize: Normalize,
    pub folded: Option<Arc<Folded>>,
    pub level: Option<PreviewLevel>,
}

impl Loaded {
    /// Memory held for this file: decoded audio and peaks.
    pub fn resident_bytes(&self) -> usize {
        let audio = match &self.source {
            Source::Whole(p) => p.samples.len() * 4,
            Source::File { .. } => 0,
        };
        let peaks = |p: &Peaks| -> usize { p.bins.iter().map(|b| b.len() * 12).sum() };
        let folded = self.prepared.as_ref().and_then(|p| p.folded.as_ref()).map(|f| peaks(&f.peaks) + f.samples.as_ref().map(|s| s.samples.len() * 4).unwrap_or(0)).unwrap_or(0);
        audio + self.peaks.as_deref().map(peaks).unwrap_or(0) + folded
    }
}

/// A file to decode, or to open for streaming.
pub struct LoadRequest {
    pub path: PathBuf,
    pub device_rate: u32,
    pub stream_above_mb: f64,
    /// The preset a decoded file's preview is made for, when the preview is on.
    pub preview: Option<ExportPreset>,
}

pub enum Job {
    Load(LoadRequest),
    Export(ExportRequest),
    /// Writes a planned copy; `ready` tells a drag in flight when the file is there.
    Copy { plan: Plan, ready: Option<Ready> },
    Scan(Vec<Root>),
}

/// A file folded to one channel: peaks for the waveform, and the samples too when the file is
/// held whole.
pub struct Folded {
    pub peaks: Arc<Peaks>,
    pub samples: Option<Arc<Pcm>>,
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
    Level { path: PathBuf, preset: ExportPreset, result: Result<PreviewLevel, String> },
    Folded { path: PathBuf, mode: DownmixMode, result: Result<Folded, String> },
}

/// A thread with one request in flight: a newer request replaces a waiting one, and `run`
/// can ask whether one has come, to give up early.
struct Latest<T> {
    next: Mutex<Option<T>>,
    wake: Condvar,
}

impl<T: Send + 'static> Latest<T> {
    /// `run` returns what to deliver, if anything, once it is done with a request.
    fn spawn(done_tx: Sender<Done>, ctx: egui::Context, run: impl Fn(T, &dyn Fn() -> bool) -> Option<Done> + Send + 'static) -> Arc<Latest<T>> {
        let slot = Arc::new(Latest { next: Mutex::new(None), wake: Condvar::new() });
        let s = slot.clone();
        std::thread::spawn(move || loop {
            let request = {
                let mut next = s.next.lock().unwrap();
                while next.is_none() {
                    next = s.wake.wait(next).unwrap();
                }
                next.take().unwrap()
            };
            let superseded = || s.next.lock().unwrap().is_some();
            let Some(done) = run(request, &superseded) else { continue };
            if done_tx.send(done).is_err() {
                return;
            }
            ctx.request_repaint();
        });
        slot
    }

    /// Returns the request this one replaced before it began.
    fn put(&self, request: T) -> Option<T> {
        let replaced = self.next.lock().unwrap().replace(request);
        self.wake.notify_one();
        replaced
    }
}

pub struct Worker {
    tx: Sender<Job>,
    rx: Receiver<Done>,
    /// The file the user is waiting on loads here, never behind the pool's preloads.
    selected: Arc<Latest<LoadRequest>>,
    analysis: Arc<Latest<(PathBuf, Option<Arc<Peaks>>)>>,
    level: Arc<Latest<(PathBuf, ExportPreset)>>,
    fold: Arc<Latest<(PathBuf, Source, DownmixMode)>>,
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
                    Job::Load(req) => load_done(req),
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
        // A load always reports, even one a newer selection overtook: the window keeps it
        // in its cache, and counts the file as pending until it hears.
        let selected = Latest::spawn(done_tx.clone(), ctx.clone(), |req: LoadRequest, _| Some(load_done(req)));
        let analysis = Latest::spawn(done_tx.clone(), ctx.clone(), |(path, cached): (PathBuf, Option<Arc<Peaks>>), abort| {
            let result = analyze(&path, cached, abort);
            (!abort()).then(|| Done::Analyzed(path, result.map_err(|e| format!("{e:#}"))))
        });
        let level = Latest::spawn(done_tx.clone(), ctx.clone(), |(path, preset): (PathBuf, ExportPreset), abort| {
            let result = hark_core::export::preview_level(&path, &preset, abort).map_err(|e| format!("{e:#}"));
            (!abort()).then_some(Done::Level { path, preset, result })
        });
        let fold = Latest::spawn(done_tx, ctx, |(path, source, mode): (PathBuf, Source, DownmixMode), abort| {
            let result = fold(&path, &source, mode, abort).map_err(|e| format!("{e:#}"));
            (!abort()).then_some(Done::Folded { path, mode, result })
        });
        Worker { tx, rx, selected, analysis, level, fold }
    }

    pub fn send(&self, job: Job) {
        let _ = self.tx.send(job);
    }

    /// Loads the file the user is waiting on. Returns the path of a request it replaced
    /// before that one began, which will never report.
    pub fn load_selected(&self, req: LoadRequest) -> Option<PathBuf> {
        self.selected.put(req).map(|r| r.path)
    }

    /// Asks for a streamed file's peaks (unless `cached`) and loudness, replacing any request
    /// still waiting.
    pub fn analyze(&self, path: PathBuf, cached: Option<Arc<Peaks>>) {
        self.analysis.put((path, cached));
    }

    /// Asks for the gain the preview plays the file at under `preset`.
    pub fn preview_level(&self, path: PathBuf, preset: ExportPreset) {
        self.level.put((path, preset));
    }

    /// Asks for the file folded to one channel, for the preview's waveform.
    pub fn fold(&self, path: PathBuf, source: Source, mode: DownmixMode) {
        self.fold.put((path, source, mode));
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

fn load_done(req: LoadRequest) -> Done {
    let result = load(&req.path, req.device_rate, req.stream_above_mb, req.preview).map(Arc::new).map_err(|e| format!("{e:#}"));
    Done::Loaded(req.path, result)
}

/// Decodes a file whole, or opens it for streaming when its decoded size is over the
/// threshold. Loudness is left to the analysis pass so playback never waits on it, except the
/// preview's level of a decoded file, which playback needs from its first frame.
fn load(path: &Path, device_rate: u32, stream_above_mb: f64, preview: Option<ExportPreset>) -> anyhow::Result<Loaded> {
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
            prepared: None,
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
    let device = Source::Whole(Arc::new(if source.sample_rate == device_rate { source.clone() } else { hark_core::resample::resample(&source, device_rate)? }));
    let prepared = preview.map(|p| Prepared {
        mono: p.mono,
        normalize: p.normalize,
        folded: p.mono.and_then(|mode| fold(path, &device, mode, &|| false).ok()).map(Arc::new),
        level: hark_core::export::preview_level_of(&source, &p).ok(),
    });
    Ok(Loaded {
        path: path.to_path_buf(),
        source_rate: source.sample_rate,
        channels: source.channels,
        duration_secs: source.duration_secs(),
        source: device,
        peaks: Some(Arc::new(peaks)),
        loudness: None,
        prepared,
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

fn fold(path: &Path, source: &Source, mode: DownmixMode, abort: &dyn Fn() -> bool) -> anyhow::Result<Folded> {
    if let Source::Whole(pcm) = source {
        let mono = pcm.to_mono(mode);
        return Ok(Folded { peaks: Arc::new(Peaks::from_pcm(&mono, Peaks::DEFAULT_BIN)), samples: Some(Arc::new(mono)) });
    }
    let mut reader = hark_core::decode::Reader::open(path)?;
    let channels = reader.channels as usize;
    let mut builder = hark_core::peaks::PeaksBuilder::new(reader.sample_rate, 1, Peaks::DEFAULT_BIN);
    let mut chunk = Vec::new();
    let mut folded = Vec::new();
    while reader.next_chunk(&mut chunk)? {
        folded.clear();
        folded.extend(chunk.chunks(channels).map(|f| mode.fold(f)));
        builder.push(&folded);
        chunk.clear();
        if abort() {
            anyhow::bail!("superseded");
        }
    }
    Ok(Folded { peaks: Arc::new(builder.finish()), samples: None })
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

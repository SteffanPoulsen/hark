//! Background threads: decoding, peaks, loudness, exports and scans, with results polled by
//! the UI once per frame.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use hark_core::audition::Source;
use hark_core::config::Root;
use hark_core::config::Normalize;
use hark_core::copies::Plan;
use hark_core::export::{ExportRequest, ExportResult};
use hark_core::library::ScanReport;
use hark_core::loudness::{Level, Span, Stamp};
use hark_core::pcm::DownmixMode;
use hark_core::peaks::{Bin, PeaksBuilder};
use hark_core::{ExportPreset, Loudness, Pcm, Peaks};

use crate::promise::Promise;

pub struct Loaded {
    pub path: PathBuf,
    /// The file as it was when it was loaded, which audio held from it is of.
    pub stamp: Stamp,
    pub source_rate: u32,
    pub channels: u16,
    pub duration_secs: f64,
    pub source: Source,
    /// `None` until a streamed file's analysis pass has run.
    pub peaks: Option<Arc<Peaks>>,
    /// The whole file's own level, measured by the analysis pass; `None` before it runs, and
    /// for a file whose loudness only the library holds.
    pub loudness: Option<Level>,
    /// A decoded file folded as the preset it was loaded with folds, so its preview never
    /// shows without it.
    pub folded: Option<(DownmixMode, Arc<Folded>)>,
}

impl Loaded {
    /// Memory held for this file: decoded audio and peaks.
    pub fn resident_bytes(&self) -> usize {
        let audio = match &self.source {
            Source::Whole(p) => p.samples.len() * 4,
            Source::File { .. } => 0,
        };
        let peaks = |p: &Peaks| -> usize { p.bins.iter().map(|b| b.len() * 12).sum() };
        let folded = self.folded.as_ref().map(|(_, f)| peaks(&f.peaks) + f.samples.as_ref().map(|s| s.samples.len() * 4).unwrap_or(0)).unwrap_or(0);
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
    /// A file the user may select next; waits while a copy is queued or being written.
    Preload(LoadRequest),
    Export(Box<ExportRequest>),
    Scan(Vec<Root>),
}

/// How many copies are queued or being written. Preloads wait while any are, so a drop's
/// file never shares the cores with guesses.
#[derive(Default)]
struct Copying {
    count: Mutex<usize>,
    idle: Condvar,
}

impl Copying {
    fn add(&self, n: isize) {
        let mut count = self.count.lock().unwrap();
        *count = count.saturating_add_signed(n);
        if *count == 0 {
            self.idle.notify_all();
        }
    }

    fn wait_idle(&self) {
        let mut count = self.count.lock().unwrap();
        while *count > 0 {
            count = self.idle.wait(count).unwrap();
        }
    }
}

/// A file folded to one channel: peaks for the waveform, and the samples too when the file is
/// held whole.
pub struct Folded {
    pub peaks: Arc<Peaks>,
    pub samples: Option<Arc<Pcm>>,
    /// The whole fold's level at the file's own rate, measured when the fold was asked to,
    /// for a preset that normalises.
    pub loudness: Option<Level>,
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
    Analyzed(PathBuf, Result<(Arc<Peaks>, Option<Level>), String>),
    Exported(Result<ExportResult, String>),
    Copied { output: PathBuf, result: Result<ExportResult, String> },
    /// One result per root, by name; a root that could not be scanned carries the reason.
    Scanned(Vec<(String, Result<ScanReport, String>)>),
    Folded { path: PathBuf, mode: DownmixMode, measure: bool, result: Result<Folded, String> },
    /// Bins an analysis (`mode` `None`) or a fold of `path` closed since its last part, from
    /// bin `from`; a part from bin 0 starts the envelope over.
    Partial { path: PathBuf, mode: Option<DownmixMode>, from: usize, sample_rate: u32, bins: Vec<Vec<Bin>> },
    /// The level of a span, for the preview's level of a selection.
    SpanMeasured(Span, Result<Level, String>),
}

/// How often a whole-file pass sends the bins it has closed.
const PARTIAL_EVERY: Duration = Duration::from_millis(200);

/// Sends a pass's new bins at most every `PARTIAL_EVERY`.
struct Parts<'a> {
    emit: &'a dyn Fn(Done),
    path: &'a Path,
    mode: Option<DownmixMode>,
    sent: usize,
    last: Instant,
}

impl Parts<'_> {
    fn offer(&mut self, builder: &PeaksBuilder, sample_rate: u32) {
        if self.last.elapsed() < PARTIAL_EVERY {
            return;
        }
        self.last = Instant::now();
        let bins: Vec<Vec<Bin>> = builder.bins().iter().map(|ch| ch[self.sent..].to_vec()).collect();
        let from = std::mem::replace(&mut self.sent, builder.bins().first().map(|b| b.len()).unwrap_or(0));
        (self.emit)(Done::Partial { path: self.path.to_path_buf(), mode: self.mode, from, sample_rate, bins });
    }
}

/// A thread with one request in flight: a newer request replaces a waiting one, and `run`
/// can ask whether one has come, to give up early.
struct Latest<T> {
    next: Mutex<Option<T>>,
    wake: Condvar,
}

impl<T: Send + 'static> Latest<T> {
    /// `run` returns what to deliver, if anything, once it is done with a request, and can
    /// deliver along the way through its third argument.
    fn spawn(done_tx: Sender<Done>, ctx: egui::Context, run: impl Fn(T, &dyn Fn() -> bool, &dyn Fn(Done)) -> Option<Done> + Send + 'static) -> Arc<Latest<T>> {
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
            let emit = |d: Done| {
                let _ = done_tx.send(d);
                ctx.request_repaint();
            };
            let Some(done) = run(request, &superseded, &emit) else { continue };
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
    /// Copies are written one at a time on a thread of their own, never behind the pool's
    /// preloads, exports and scans.
    copy_tx: Sender<CopyJob>,
    copying: Arc<Copying>,
    /// The file the user is waiting on loads here, never behind the pool's preloads.
    selected: Arc<Latest<LoadRequest>>,
    analysis: Arc<Latest<(PathBuf, Option<Arc<Peaks>>)>>,
    fold: Arc<Latest<FoldJob>>,
    span: Arc<Latest<SpanJob>>,
}

struct FoldJob {
    path: PathBuf,
    source: Source,
    source_rate: u32,
    stamp: Stamp,
    mode: DownmixMode,
    measure: bool,
}

struct SpanJob {
    span: Span,
    held: Option<(Arc<Pcm>, Stamp)>,
    level: Promise<Option<Level>>,
}

struct CopyJob {
    plan: Plan,
    measuring: Option<Promise<Option<Level>>>,
}

/// How long a copy waits on a measurement of its span before measuring for itself; a running
/// one always settles sooner, so this only guards against one that never does.
const MEASURING_WAIT: Duration = Duration::from_secs(600);

impl Worker {
    pub fn start(ctx: egui::Context, threads: usize) -> Worker {
        let (tx, job_rx) = channel::<Job>();
        let (done_tx, rx) = channel::<Done>();
        let job_rx = Arc::new(Mutex::new(job_rx));
        let copying = Arc::new(Copying::default());
        for _ in 0..threads {
            let job_rx = job_rx.clone();
            let done_tx = done_tx.clone();
            let ctx = ctx.clone();
            let copying = copying.clone();
            std::thread::spawn(move || loop {
                let job = match job_rx.lock().unwrap().recv() {
                    Ok(j) => j,
                    Err(_) => return,
                };
                let done = match job {
                    Job::Preload(req) => {
                        copying.wait_idle();
                        load_done(req)
                    }
                    Job::Export(req) => Done::Exported(hark_core::export::run(&req).map_err(|e| format!("{e:#}"))),
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
        let (copy_tx, copy_rx) = channel::<CopyJob>();
        {
            let done_tx = done_tx.clone();
            let ctx = ctx.clone();
            let copying = copying.clone();
            std::thread::spawn(move || {
                for CopyJob { mut plan, measuring } in copy_rx {
                    if let (Some(export), Some(Some(level))) = (&mut plan.pending, measuring.and_then(|m| m.wait(MEASURING_WAIT))) {
                        export.levels.push(level);
                    }
                    // A panic in the pipeline fails this copy and leaves the thread for the next.
                    let result = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| hark_core::copies::finish(&plan))) {
                        Ok(r) => r.map_err(|e| format!("{e:#}")),
                        Err(_) => Err("the export stopped on an internal error".to_string()),
                    };
                    copying.add(-1);
                    if done_tx.send(Done::Copied { output: plan.output, result }).is_err() {
                        return;
                    }
                    ctx.request_repaint();
                }
            });
        }
        // A load always reports, even one a newer selection overtook: the window keeps it
        // in its cache, and counts the file as pending until it hears.
        let selected = Latest::spawn(done_tx.clone(), ctx.clone(), |req: LoadRequest, _, _| Some(load_done(req)));
        let analysis = Latest::spawn(done_tx.clone(), ctx.clone(), |(path, cached): (PathBuf, Option<Arc<Peaks>>), abort, emit| {
            let result = analyze(&path, cached, abort, emit);
            (!abort()).then(|| Done::Analyzed(path, result.map_err(|e| format!("{e:#}"))))
        });
        let fold = Latest::spawn(done_tx.clone(), ctx.clone(), |job: FoldJob, abort, emit| {
            let FoldJob { path, source, source_rate, stamp, mode, measure } = job;
            let result = fold(&path, &source, source_rate, stamp, mode, measure, abort, emit).map_err(|e| format!("{e:#}"));
            (!abort()).then_some(Done::Folded { path, mode, measure, result })
        });
        let span = Latest::spawn(done_tx, ctx, |job: SpanJob, abort, _| {
            let result = measure_span(&job.span, job.held, abort).map_err(|e| format!("{e:#}"));
            job.level.set(result.as_ref().ok().filter(|_| !abort()).cloned());
            (!abort()).then_some(Done::SpanMeasured(job.span, result))
        });
        Worker { tx, rx, copy_tx, copying, selected, analysis, fold, span }
    }

    pub fn send(&self, job: Job) {
        let _ = self.tx.send(job);
    }

    /// Writes a planned copy. `measuring` is a measurement of the span still running, which
    /// the copy waits for rather than measuring the same audio again. The plan comes back
    /// when the copy thread is gone and the copy will never be written.
    pub fn copy(&self, plan: Plan, measuring: Option<Promise<Option<Level>>>) -> Result<(), Box<Plan>> {
        self.copying.add(1);
        self.copy_tx.send(CopyJob { plan, measuring }).map_err(|e| {
            self.copying.add(-1);
            Box::new(e.0.plan)
        })
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

    /// Asks for the file folded to one channel, for the preview's waveform, and with
    /// `measure` its loudness, for the preview's level.
    pub fn fold(&self, file: &Loaded, mode: DownmixMode, measure: bool) {
        self.fold.put(FoldJob { path: file.path.clone(), source: file.source.clone(), source_rate: file.source_rate, stamp: file.stamp, mode, measure });
    }

    /// Asks for the level of `span`, for the preview's level of a selection: metered from
    /// `held`, the file's samples at its own rate already folded as the span says, with the
    /// stamp of the file they were read from, or streamed from the file without them. Replaces
    /// any request still waiting. The promise is kept with the level, or with `None` when the
    /// measurement fails or is replaced.
    pub fn measure_span(&self, span: Span, held: Option<(Arc<Pcm>, Stamp)>) -> Promise<Option<Level>> {
        let level = Promise::new();
        if let Some(replaced) = self.span.put(SpanJob { span, held, level: level.clone() }) {
            replaced.level.set(None);
        }
        level
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
    let stamp = Stamp::of(path)?;
    let info = hark_core::probe(path)?;
    if info.decoded_mb() > stream_above_mb {
        let peaks = peaks_cache_path(path).and_then(|c| Peaks::load(&c).ok()).map(Arc::new);
        return Ok(Loaded {
            path: path.to_path_buf(),
            stamp,
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
            folded: None,
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
    let measure = preview.as_ref().is_some_and(|p| !matches!(p.normalize, Normalize::None));
    let mono = preview.and_then(|p| p.mono);
    // The fold plays at the device's rate; its level is taken at the file's, from the audio
    // already in hand.
    let folded = mono.and_then(|mode| {
        let mut f = fold(path, &device, source.sample_rate, stamp, mode, false, &|| false, &|_| {}).ok()?;
        f.loudness = measure.then(|| hark_core::loudness::measure(&source.to_mono(mode)).ok().map(|l| whole(path, stamp, Some(mode), source.frames(), l))).flatten();
        Some((mode, Arc::new(f)))
    });
    let loudness = (measure && mono.is_none()).then(|| hark_core::loudness::measure(&source).ok().map(|l| whole(path, stamp, None, source.frames(), l))).flatten();
    Ok(Loaded {
        path: path.to_path_buf(),
        stamp,
        source_rate: source.sample_rate,
        channels: source.channels,
        duration_secs: source.duration_secs(),
        source: device,
        peaks: Some(Arc::new(peaks)),
        loudness,
        folded,
    })
}

/// The level of all of a file at its own rate, folded by `fold`, measured over `frames` of it
/// as it was when `stamp` was taken.
fn whole(path: &Path, stamp: Stamp, fold: Option<DownmixMode>, frames: usize, loudness: Loudness) -> Level {
    Level { span: Span { path: path.to_path_buf(), secs: None, fades: Default::default(), fold, rate: None }, stamp, frames: (0, frames as u64), loudness }
}

/// One pass over a streamed file for its peaks (unless cached) and loudness, holding only a
/// packet at a time; gives up as soon as `abort` says so.
fn analyze(path: &Path, cached: Option<Arc<Peaks>>, abort: &dyn Fn() -> bool, emit: &dyn Fn(Done)) -> anyhow::Result<(Arc<Peaks>, Option<Level>)> {
    let stamp = Stamp::of(path)?;
    let mut reader = hark_core::decode::Reader::open(path)?;
    let mut chunk = Vec::new();
    let mut builder = None;
    let mut meter = None;
    let mut frames = 0;
    let mut parts = Parts { emit, path, mode: None, sent: 0, last: Instant::now() };
    while reader.next_chunk(&mut chunk)? {
        frames += chunk.len() / (reader.channels as usize).max(1);
        if cached.is_none() {
            let b = builder.get_or_insert_with(|| PeaksBuilder::new(reader.sample_rate, reader.channels, Peaks::DEFAULT_BIN));
            b.push(&chunk);
            parts.offer(b, reader.sample_rate);
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
    Ok((peaks, meter.and_then(|m| m.finish()).map(|l| whole(path, stamp, None, frames, l))))
}

/// `source` folded to one channel at its own rate, which for a decoded file is the device's;
/// its level is the file's at the file's own rate, `source_rate`. `stamp` is when a decoded
/// file was read.
#[allow(clippy::too_many_arguments)]
fn fold(path: &Path, source: &Source, source_rate: u32, stamp: Stamp, mode: DownmixMode, measure: bool, abort: &dyn Fn() -> bool, emit: &dyn Fn(Done)) -> anyhow::Result<Folded> {
    if let Source::Whole(pcm) = source {
        let mono = pcm.to_mono(mode);
        let loudness = match measure {
            false => None,
            true if pcm.sample_rate == source_rate => Some(whole(path, stamp, Some(mode), mono.frames(), hark_core::loudness::measure(&mono)?)),
            true => Some(hark_core::loudness::measure_file(&Span { path: path.to_path_buf(), secs: None, fades: Default::default(), fold: Some(mode), rate: None }, abort)?),
        };
        return Ok(Folded { peaks: Arc::new(Peaks::from_pcm(&mono, Peaks::DEFAULT_BIN)), samples: Some(Arc::new(mono)), loudness });
    }
    let stamp = Stamp::of(path)?;
    let mut reader = hark_core::decode::Reader::open(path)?;
    let channels = reader.channels as usize;
    let mut builder = PeaksBuilder::new(reader.sample_rate, 1, Peaks::DEFAULT_BIN);
    let mut meter = if measure { Some(hark_core::loudness::Meter::new(1, reader.sample_rate)?) } else { None };
    let mut parts = Parts { emit, path, mode: Some(mode), sent: 0, last: Instant::now() };
    let mut chunk = Vec::new();
    let mut folded = Vec::new();
    let mut frames = 0;
    while reader.next_chunk(&mut chunk)? {
        frames += chunk.len() / channels.max(1);
        folded.clear();
        folded.extend(chunk.chunks(channels).map(|f| mode.fold(f)));
        builder.push(&folded);
        parts.offer(&builder, reader.sample_rate);
        if let Some(m) = &mut meter {
            m.push(&folded)?;
        }
        chunk.clear();
        if abort() {
            anyhow::bail!("superseded");
        }
    }
    Ok(Folded { peaks: Arc::new(builder.finish()), samples: None, loudness: meter.and_then(|m| m.finish()).map(|l| whole(path, stamp, Some(mode), frames, l)) })
}

/// The level of a span from samples in memory, read from the file when it was as `stamp`
/// says, or streamed from its file; gives up as soon as `abort` says so.
fn measure_span(span: &Span, held: Option<(Arc<Pcm>, Stamp)>, abort: &dyn Fn() -> bool) -> anyhow::Result<Level> {
    let Some((pcm, stamp)) = held else { return hark_core::loudness::measure_file(span, abort) };
    let frames = match span.secs {
        Some((a, b)) => (pcm.frame_at(a), pcm.frame_at(b)),
        None => (0, pcm.frames()),
    };
    let mut cut = pcm.slice_frames(frames.0, frames.1);
    span.fades.apply(&mut cut);
    let loudness = hark_core::loudness::measure_with(&cut, abort)?;
    Ok(Level { span: span.clone(), stamp, frames: (frames.0 as u64, frames.1 as u64), loudness })
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

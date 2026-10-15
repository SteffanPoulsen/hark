//! Background threads: decoding, peaks, loudness, exports and scans, with results polled by
//! the UI once per frame.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};

use hark_core::audition::Source;
use hark_core::config::Root;
use hark_core::export::{ExportRequest, ExportResult};
use hark_core::library::ScanReport;
use hark_core::{Loudness, Peaks};

pub struct Loaded {
    pub path: PathBuf,
    pub source_rate: u32,
    pub channels: u16,
    pub duration_secs: f64,
    pub source: Source,
    /// `None` while a streamed file's analysis pass is still running.
    pub peaks: Option<Arc<Peaks>>,
    /// Measured by this load; a streamed file whose loudness the library already holds
    /// leaves it `None`.
    pub loudness: Option<Loudness>,
}

impl Loaded {
    /// Frames held in memory at the device rate; a streamed file holds none.
    pub fn resident_frames(&self) -> usize {
        match &self.source {
            Source::Whole(p) => p.frames(),
            Source::File { .. } => 0,
        }
    }
}

pub enum Job {
    Load {
        path: PathBuf,
        device_rate: u32,
        stream_above_secs: f64,
        /// The library already holds the file's loudness, so a streamed file with cached peaks
        /// needs no analysis pass.
        analyzed: bool,
    },
    Export(ExportRequest),
    Scan(Vec<Root>),
}

pub enum Done {
    Loaded(PathBuf, Result<Arc<Loaded>, String>),
    /// The analysis pass of a streamed file, after its `Loaded`.
    Analyzed(PathBuf, Arc<Peaks>, Option<Loudness>),
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
            std::thread::spawn(move || {
                let emit = |done: Done| {
                    let alive = done_tx.send(done).is_ok();
                    ctx.request_repaint();
                    alive
                };
                loop {
                    let job = match job_rx.lock().unwrap().recv() {
                        Ok(j) => j,
                        Err(_) => return,
                    };
                    let alive = match job {
                        Job::Load { path, device_rate, stream_above_secs, analyzed } => {
                            let loaded = load(&path, device_rate, stream_above_secs).map(Arc::new).map_err(|e| format!("{e:#}"));
                            let pass = loaded.as_ref().ok().filter(|l| l.source.is_streamed() && (l.peaks.is_none() || !analyzed)).map(|l| l.peaks.clone());
                            emit(Done::Loaded(path.clone(), loaded))
                                && match pass {
                                    Some(cached) => match analyze(&path, cached) {
                                        Ok((peaks, loudness)) => emit(Done::Analyzed(path, peaks, loudness)),
                                        Err(e) => emit(Done::Loaded(path.clone(), Err(format!("{e:#}")))),
                                    },
                                    None => true,
                                }
                        }
                        Job::Export(req) => emit(Done::Exported(hark_core::export::run(&req).map_err(|e| format!("{e:#}")))),
                        Job::Scan(roots) => emit(Done::Scanned(scan(&roots))),
                    };
                    if !alive {
                        return;
                    }
                }
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

fn load(path: &Path, device_rate: u32, stream_above_secs: f64) -> anyhow::Result<Loaded> {
    let info = hark_core::probe(path)?;
    if info.duration_secs > stream_above_secs {
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
    let loudness = hark_core::loudness::measure(&source).ok();
    let device = if source.sample_rate == device_rate { source.clone() } else { hark_core::resample::resample(&source, device_rate)? };
    Ok(Loaded {
        path: path.to_path_buf(),
        source_rate: source.sample_rate,
        channels: source.channels,
        duration_secs: source.duration_secs(),
        source: Source::Whole(Arc::new(device)),
        peaks: Some(Arc::new(peaks)),
        loudness,
    })
}

/// One pass over a streamed file for its peaks (unless cached) and loudness, holding only a
/// packet at a time.
fn analyze(path: &Path, cached: Option<Arc<Peaks>>) -> anyhow::Result<(Arc<Peaks>, Option<Loudness>)> {
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

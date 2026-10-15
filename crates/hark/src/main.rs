#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod preset_ui;
mod settings;
mod theme;
mod waveform;
mod worker;

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{Color32, CornerRadius, FontId, Key, Margin, RichText, Sense, Stroke, StrokeKind};
use egui_extras::{Column, TableBuilder};
use egui_phosphor::regular as icons;
use hark_core::audition::{Clip, Player, Source};
use hark_core::export::{ExportRequest, Selection};
use hark_core::library::{ChannelClass, Query};
use hark_core::{Config, Db, ExportPreset, FileRecord, Loudness};

use theme::Theme;
use waveform::{WaveEvent, WaveState};
use worker::{Done, Job, Loaded, Worker};

/// Twenty minutes of stereo at 48 kHz, decoded audio and peaks together.
const CACHE_BYTES: usize = 48_000 * 60 * 20 * 2 * 4;
const PRELOAD: usize = 3;
const EXPORT_CARD_W: f32 = 380.0;
/// Height of the top bar's row of controls; the bar is this plus its margins.
const TOP_BAR_H: f32 = 32.0;
/// Height of every control in the top bar and the transport row, so each reads as one line.
const CONTROL_H: f32 = 28.0;
/// Search results are cut here so typing stays quick; folder and favorites views are whole.
const SEARCH_LIMIT: usize = 2000;

/// File types offered by the filter, each with the extensions it stands for.
const FILE_TYPES: [(&str, &[&str]); 7] = [
    ("wav", &["wav", "wave"]),
    ("flac", &["flac"]),
    ("ogg", &["ogg", "oga"]),
    ("mp3", &["mp3"]),
    ("aiff", &["aif", "aiff", "aifc"]),
    ("m4a", &["m4a", "mp4"]),
    ("opus", &["opus"]),
];

/// A path for display, with the home directory as `~`.
fn tilde(path: &Path) -> String {
    match std::env::home_dir().and_then(|h| path.strip_prefix(h).ok().map(|r| r.to_path_buf())) {
        Some(rest) if rest.as_os_str().is_empty() => "~".into(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

fn main() -> eframe::Result {
    let mut open: Option<PathBuf> = None;
    let mut project: Option<PathBuf> = None;
    let mut settings: Option<settings::Page> = None;
    let mut region = OpenRegion::default();
    let mut args = std::env::args().skip(1);
    let secs = |args: &mut std::iter::Skip<std::env::Args>| args.next().and_then(|v| v.parse::<f64>().ok());
    while let Some(a) = args.next() {
        match a.as_str() {
            "--open" => open = args.next().map(PathBuf::from),
            "--project" => project = args.next().map(PathBuf::from),
            "--in" => region.start = secs(&mut args),
            "--out" => region.end = secs(&mut args),
            "--fade-in" => region.fade_in = secs(&mut args).unwrap_or(0.0),
            "--fade-out" => region.fade_out = secs(&mut args).unwrap_or(0.0),
            "--settings" => {
                settings = Some(match args.next().as_deref() {
                    Some("tags") => settings::Page::Tags,
                    Some("presets") | Some("export") => settings::Page::Presets,
                    Some("general") | None => settings::Page::General,
                    Some(other) => {
                        open = Some(PathBuf::from(other));
                        settings::Page::General
                    }
                })
            }
            "-h" | "--help" => {
                println!("hark [--open <audio file or folder>] [--in <secs>] [--out <secs>] [--fade-in <secs>] [--fade-out <secs>] [--project <file.hark>] [--settings [general|tags|presets]]");
                return Ok(());
            }
            other => open = Some(PathBuf::from(other)),
        }
    }
    let open = open.and_then(|p| std::fs::canonicalize(p).ok());
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("hark")
            .with_app_id("hark")
            .with_icon(eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon/hark-256.png")).expect("the icon is a PNG"))
            .with_inner_size([1500.0, 900.0])
            .with_min_inner_size([900.0, 600.0]),
        ..Default::default()
    };
    eframe::run_native(
        "hark",
        options,
        Box::new(move |cc| {
            let mut app = App::new(cc);
            if let Some(p) = project {
                app.apply_project_action(ProjectAction::Open(p));
            }
            if let Some(p) = open {
                app.open_region = Some((p.clone(), region));
                app.open_path(&p);
            }
            if let Some(page) = settings {
                app.settings_open = true;
                app.settings_page = page;
            }
            Ok(Box::new(app))
        }),
    )
}

/// A selection and fades asked for on the command line, applied once the opened file loads.
#[derive(Default, Clone, Copy)]
struct OpenRegion {
    start: Option<f64>,
    end: Option<f64>,
    fade_in: f64,
    fade_out: f64,
}

enum ProjectAction {
    /// Back to the default state.
    Default,
    Open(PathBuf),
    /// Pick a project file.
    Browse,
    /// Save a new project file.
    New,
    Forget(PathBuf),
}

enum TagAction {
    Color(usize, String),
    Rename(usize, String),
    Move(usize, usize),
    Remove(usize),
    Add,
}

/// What the list shows when nothing is being searched.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Location {
    Favorites,
    Folder(PathBuf),
}

/// Narrows the list, whatever it shows; each axis is a set, and an empty set leaves it open.
#[derive(Default)]
struct Filter {
    favorites: bool,
    tags: Vec<String>,
    types: Vec<&'static str>,
    channels: Vec<ChannelClass>,
}

impl Filter {
    fn count(&self) -> usize {
        self.favorites as usize + self.tags.len() + self.types.len() + self.channels.len()
    }

    fn clear(&mut self) {
        *self = Filter::default();
    }

    fn extensions(&self) -> Vec<String> {
        FILE_TYPES.iter().filter(|(name, _)| self.types.contains(name)).flat_map(|(_, exts)| exts.iter().map(|e| e.to_string())).collect()
    }
}

fn toggle_in<T: PartialEq>(set: &mut Vec<T>, item: T) {
    match set.iter().position(|x| *x == item) {
        Some(i) => {
            set.remove(i);
        }
        None => set.push(item),
    }
}

struct App {
    config: Config,
    theme: Theme,
    db: Db,
    player: Option<Player>,
    audio_error: Option<String>,
    worker: Worker,

    expanded: HashSet<PathBuf>,
    dir_children: HashMap<PathBuf, Vec<PathBuf>>,
    location: Option<Location>,
    fav_count: usize,

    search: String,
    filter: Filter,
    files: Vec<FileRecord>,
    selected: Option<usize>,
    list_dirty: bool,
    scroll_to_selected: bool,

    cache: HashMap<PathBuf, Arc<Loaded>>,
    cache_order: VecDeque<PathBuf>,
    pending: HashSet<PathBuf>,
    current: Option<Arc<Loaded>>,
    wave: WaveState,
    open_region: Option<(PathBuf, OpenRegion)>,

    autoplay: bool,
    loop_on: bool,
    transport_h: f32,
    /// Height the export card's content took on the last frame; the bottom panel is sized to it.
    export_card_h: f32,
    gain_db: f32,
    /// The preset the card's values came from; `None` once they were edited.
    preset_name: Option<String>,
    /// The values the next export uses.
    preset: ExportPreset,
    overwrite: bool,
    status: String,
    status_since: Instant,
    status_is_error: bool,
    scanning: bool,
    settings_open: bool,
    settings_page: settings::Page,
    /// The mark beside the wordmark, white on transparent, tinted at draw time.
    mark: egui::TextureHandle,
    /// Centre line of the project switcher on the last frame; the brand sits on it, since
    /// egui does not centre buttons on the row they are in.
    search_id: egui::Id,
    tag_edit: Option<(usize, String)>,
    /// Playback was running when the device was reopened; resume once the file reloads.
    play_on_load: bool,
    /// Output file name for the next export; empty takes the preset's template.
    export_name: String,
    /// The preset the settings page edits.
    preset_edit: String,
    preset_rename: Option<String>,
    file_count: usize,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let config = Config::load().unwrap_or_else(|e| {
            eprintln!("config: {e:#}");
            Config::default()
        });
        let theme_file = std::env::var_os("HARK_THEME").map(PathBuf::from).or_else(|| config.theme_file.clone());
        let theme = theme_file.as_ref().and_then(|p| Theme::from_file(p)).unwrap_or_default();
        theme.apply(&cc.egui_ctx);
        cc.egui_ctx.set_zoom_factor(config.ui_zoom.clamp(0.5, 3.0));
        let mut db = Db::open().expect("library database");
        db.set_project(config.project_id());
        let (player, audio_error) = match Player::open(config.audition_sample_rate) {
            Ok(p) => (Some(p), None),
            Err(e) => (None, Some(format!("{e:#}"))),
        };
        let worker = Worker::start(cc.egui_ctx.clone(), 3);
        let mark = {
            let png = image::load_from_memory(include_bytes!("../assets/icon/hark-mark-96.png")).expect("the mark is a PNG").into_rgba8();
            let (w, h) = png.dimensions();
            cc.egui_ctx.load_texture("mark", egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], png.as_raw()), egui::TextureOptions::LINEAR)
        };
        if !config.roots().is_empty() {
            worker.send(Job::Scan(config.roots().to_vec()));
        }
        let preset_name = config.default_preset.clone();
        let preset = config.preset(&preset_name).cloned().unwrap_or_default();
        let scanning = !config.roots().is_empty();
        let expanded = config.roots().iter().map(|r| r.path.clone()).collect();
        App {
            config,
            theme,
            db,
            player,
            audio_error,
            worker,
            expanded,
            dir_children: HashMap::new(),
            location: None,
            fav_count: 0,
            search: String::new(),
            filter: Filter::default(),
            files: Vec::new(),
            selected: None,
            list_dirty: true,
            scroll_to_selected: false,
            cache: HashMap::new(),
            cache_order: VecDeque::new(),
            pending: HashSet::new(),
            current: None,
            wave: WaveState::default(),
            open_region: None,
            autoplay: true,
            loop_on: false,
            transport_h: 34.0,
            export_card_h: 380.0,
            gain_db: 0.0,
            preset_name: Some(preset_name.clone()),
            preset,
            overwrite: false,
            status: String::new(),
            status_since: Instant::now(),
            status_is_error: false,
            scanning,
            file_count: 0,
            settings_open: false,
            mark,
            settings_page: settings::Page::General,
            search_id: egui::Id::new("search"),
            tag_edit: None,
            play_on_load: false,
            export_name: String::new(),
            preset_edit: preset_name,
            preset_rename: None,
        }
    }

    /// Shows a folder, or a file's folder with the file selected, expanding the tree to it.
    fn open_path(&mut self, path: &Path) {
        let dir = if path.is_dir() { path.to_path_buf() } else { path.parent().map(|p| p.to_path_buf()).unwrap_or_default() };
        let mut p = dir.clone();
        loop {
            self.expanded.insert(p.clone());
            match p.parent() {
                Some(parent) if self.config.roots().iter().all(|r| p != r.path) && parent != Path::new("/") => p = parent.to_path_buf(),
                _ => break,
            }
        }
        self.location = Some(Location::Folder(dir));
        self.refresh_list();
        if path.is_file() {
            if let Some(i) = self.files.iter().position(|f| f.path == path) {
                self.select(i, false);
            }
        }
    }

    /// A message holds the footer for a while, then the key hints come back.
    /// Shrinks the selection, or the whole file, to the sound in it.
    fn trim_selection(&mut self) {
        let Some(cur) = &self.current else { return };
        let Source::Whole(pcm) = &cur.source else {
            self.set_status("Trim needs the file decoded whole; this one streams", true);
            return;
        };
        let (start, end) = self.wave.selection.unwrap_or((0.0, cur.duration_secs));
        match pcm.sound_span(start, end) {
            Some(span) => self.wave.selection = Some(span),
            None => self.set_status("Silent; nothing to trim to", true),
        }
    }

    fn set_status(&mut self, msg: impl Into<String>, error: bool) {
        self.status = msg.into();
        self.status_is_error = error;
        self.status_since = Instant::now();
    }

    fn expire_status(&mut self, ctx: &egui::Context) {
        if self.status.is_empty() {
            return;
        }
        let hold = Duration::from_secs(if self.status_is_error { 20 } else { 8 });
        match hold.checked_sub(self.status_since.elapsed()) {
            Some(left) => ctx.request_repaint_after(left),
            None => self.status.clear(),
        }
    }

    fn children_of(&mut self, dir: &Path) -> Vec<PathBuf> {
        if let Some(c) = self.dir_children.get(dir) {
            return c.clone();
        }
        let mut dirs: Vec<PathBuf> = std::fs::read_dir(dir)
            .map(|rd| rd.filter_map(|e| e.ok()).filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false)).map(|e| e.path()).collect())
            .unwrap_or_default();
        dirs.sort_by_key(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()));
        self.dir_children.insert(dir.to_path_buf(), dirs.clone());
        dirs
    }

    fn refresh_list(&mut self) {
        self.list_dirty = false;
        self.fav_count = self.db.favorite_count(self.db.project()).unwrap_or(0);
        let mut q = Query {
            favorites_only: self.filter.favorites,
            tags: self.filter.tags.clone(),
            extensions: self.filter.extensions(),
            channels: self.filter.channels.clone(),
            ..Query::default()
        };
        if !self.search.trim().is_empty() {
            q.text = self.search.clone();
            q.scope = self.config.roots().iter().map(|r| r.path.clone()).collect();
            q.limit = SEARCH_LIMIT;
        } else {
            match &self.location {
                Some(Location::Folder(d)) => q.dir = Some(d.clone()),
                Some(Location::Favorites) => {
                    q.favorites_only = true;
                    q.scope = self.config.roots().iter().map(|r| r.path.clone()).collect();
                }
                None => {
                    self.files.clear();
                    self.selected = None;
                    return;
                }
            }
        }
        let files = match self.db.search(&q) {
            Ok(f) => f,
            Err(e) => {
                self.set_status(format!("Library: {e:#}"), true);
                Vec::new()
            }
        };
        let keep = self.selected.and_then(|i| self.files.get(i)).map(|f| f.path.clone());
        self.files = files;
        self.selected = keep.and_then(|p| self.files.iter().position(|f| f.path == p));
    }

    fn refresh_record(&mut self, i: usize) {
        let Some(path) = self.files.get(i).map(|f| f.path.clone()) else { return };
        if let Ok(Some(r)) = self.db.get(&path) {
            self.files[i] = r;
        }
    }

    fn device_rate(&self) -> u32 {
        self.player.as_ref().map(|p| p.sample_rate).unwrap_or(48_000)
    }

    fn request_load(&mut self, path: &Path) {
        if self.cache.contains_key(path) || self.pending.contains(path) {
            return;
        }
        self.pending.insert(path.to_path_buf());
        self.worker.send(Job::Load { path: path.to_path_buf(), device_rate: self.device_rate(), stream_above_mb: self.config.stream_above_mb });
    }

    fn remember(&mut self, loaded: Arc<Loaded>) {
        let path = loaded.path.clone();
        self.cache_order.retain(|p| p != &path);
        self.cache_order.push_back(path.clone());
        self.cache.insert(path, loaded);
        let mut bytes: usize = self.cache.values().map(|l| l.resident_bytes()).sum();
        while bytes > CACHE_BYTES && self.cache_order.len() > 1 {
            let Some(old) = self.cache_order.pop_front() else { break };
            if self.current.as_ref().map(|c| c.path == old).unwrap_or(false) {
                self.cache_order.push_back(old);
                if self.cache_order.len() == 1 {
                    break;
                }
                continue;
            }
            if let Some(l) = self.cache.remove(&old) {
                bytes -= l.resident_bytes();
            }
        }
    }

    fn select(&mut self, i: usize, play: bool) {
        if i >= self.files.len() {
            return;
        }
        self.selected = Some(i);
        self.scroll_to_selected = true;
        let path = self.files[i].path.clone();
        let is_new = self.current.as_ref().map(|c| c.path != path).unwrap_or(true);
        if is_new {
            self.export_name.clear();
            if let Some(l) = self.cache.get(&path).cloned() {
                self.show_loaded(l);
                if play {
                    self.play(None);
                }
            } else {
                self.request_load(&path);
                self.current = None;
                self.wave.reset(self.files[i].info.as_ref().map(|x| x.duration_secs).unwrap_or(1.0));
            }
        } else if play {
            self.play(None);
        }
        for d in 1..=PRELOAD {
            if let Some(f) = self.files.get(i + d) {
                self.request_load(&f.path.clone());
            }
            if i >= d {
                if let Some(f) = self.files.get(i - d) {
                    self.request_load(&f.path.clone());
                }
            }
        }
    }

    fn show_loaded(&mut self, l: Arc<Loaded>) {
        let duration = l.duration_secs;
        self.wave.reset(duration);
        if let Some((_, r)) = self.open_region.take_if(|(p, _)| *p == l.path) {
            let (a, b) = (r.start.unwrap_or(0.0).clamp(0.0, duration), r.end.unwrap_or(duration).clamp(0.0, duration));
            if b > a && (r.start.is_some() || r.end.is_some()) {
                self.wave.selection = Some((a, b));
            }
            self.wave.fade_in = r.fade_in.max(0.0);
            self.wave.fade_out = r.fade_out.max(0.0);
        }
        let measured = l.loudness.is_some() || self.files.iter().find(|f| f.path == l.path).map(|f| f.lufs.is_some()).unwrap_or(false);
        if l.peaks.is_none() || !measured {
            self.worker.analyze(l.path.clone(), l.peaks.clone());
        }
        self.current = Some(l);
    }

    fn play(&mut self, from_secs: Option<f64>) {
        let (Some(player), Some(cur)) = (&self.player, &self.current) else { return };
        let rate = player.sample_rate as f64;
        let total = cur.source.frames(player.sample_rate);
        let frames = |t: f64| ((t * rate) as usize).min(total);
        // The fades belong to the selection, or to the whole file without one. A seek outside
        // the selection auditions the file from there without them.
        let (start, end) = match self.wave.selection {
            Some((a, b)) => (frames(a), frames(b)),
            None => (0, total),
        };
        let from = from_secs.map(frames).unwrap_or(start);
        let inside = from >= start && from < end;
        let (start, end) = if inside { (start, end) } else { (from, total) };
        let (fade_in, fade_out) = if inside { (frames(self.wave.fade_in), frames(self.wave.fade_out)) } else { (0, 0) };
        player.play_from(
            Clip { source: cur.source.clone(), start, end: end.max(start), looped: self.loop_on, fade_in, fade_out, fade_shape: self.wave.fade_shape },
            from,
        );
    }

    fn stop(&self) {
        if let Some(p) = &self.player {
            p.stop();
        }
    }

    fn is_playing(&self) -> bool {
        self.player.as_ref().map(|p| p.is_playing()).unwrap_or(false)
    }

    fn toggle_favorite(&mut self) {
        if let Some(i) = self.selected {
            self.toggle_favorite_of(i);
        }
    }

    fn toggle_favorite_of(&mut self, i: usize) {
        let path = self.files[i].path.clone();
        let on = !self.files[i].favorite;
        match self.db.ensure_hash(&path).and_then(|h| self.db.set_favorite(&h, on)) {
            Ok(()) => {
                self.refresh_record(i);
                self.fav_count = self.db.favorite_count(self.db.project()).unwrap_or(0);
            }
            Err(e) => self.set_status(format!("Favorite: {e:#}"), true),
        }
    }

    fn toggle_tag(&mut self, tag: &str) {
        if let Some(i) = self.selected {
            self.toggle_tag_of(i, tag);
        }
    }

    fn toggle_tag_of(&mut self, i: usize, tag: &str) {
        let path = self.files[i].path.clone();
        let on = !self.files[i].tags.iter().any(|t| t == tag);
        match self.db.ensure_hash(&path).and_then(|h| self.db.set_tag(&h, tag, on)) {
            Ok(()) => self.refresh_record(i),
            Err(e) => self.set_status(format!("Tag: {e:#}"), true),
        }
    }


    fn export_request(&self) -> Option<Result<ExportRequest, String>> {
        let i = self.selected?;
        let f = &self.files[i];
        let mut req = match ExportRequest::from_config(&self.config, &f.path, None, f.tags.clone(), f.favorite) {
            Ok(r) => r,
            Err(e) => return Some(Err(format!("{e:#}"))),
        };
        req.preset = self.preset.clone();
        req.preset_name = self.preset_name.clone();
        req.selection = self.wave.selection.map(|(a, b)| Selection { start_secs: a, end_secs: b });
        req.fade_in_secs = self.wave.fade_in;
        req.fade_out_secs = self.wave.fade_out;
        req.gain_db = self.gain_db as f64;
        req.preset.fade_shape = self.wave.fade_shape;
        req.overwrite = self.overwrite;
        let name: String = self.export_name.trim().chars().filter(|c| !matches!(c, '/' | '\\')).collect();
        if !name.is_empty() {
            let templated = req.output_path();
            req.output_path = Some(templated.with_file_name(format!("{name}.{}", req.preset.format.extension())));
        }
        Some(Ok(req))
    }

    /// What the export would write: its length and, for WAV, the rough size.
    fn export_outcome(&self, req: &ExportRequest, f: &FileRecord) -> String {
        let Some(info) = &f.info else { return String::new() };
        let preset = req.effective_preset();
        let secs = req.selection.map(|s| s.end_secs - s.start_secs).unwrap_or(info.duration_secs);
        let mut out = format!("{} s", waveform::fmt_time(secs));
        if preset.format == hark_core::config::OutputFormat::Wav {
            let channels = if preset.mono.is_some() { 1 } else { info.channels };
            let rate = preset.sample_rate.unwrap_or(info.sample_rate);
            let bytes = secs * rate as f64 * channels as f64 * preset.bits as f64 / 8.0;
            out.push_str(&if bytes >= 1e6 { format!("  ·  ≈ {:.1} MB", bytes / 1e6) } else { format!("  ·  ≈ {:.0} KB", bytes / 1e3) });
        }
        out
    }

    fn export(&mut self) {
        match self.export_request() {
            Some(Ok(req)) => {
                self.set_status(format!("Exporting {}", req.output_path().display()), false);
                self.worker.send(Job::Export(req));
            }
            Some(Err(e)) => self.set_status(format!("Export: {e}"), true),
            None => {}
        }
    }

    fn record_loudness(&mut self, path: &Path, loudness: Option<Loudness>) {
        let Some(ld) = loudness else { return };
        let _ = self.db.set_loudness(path, ld.integrated_lufs, ld.true_peak_dbfs);
        if let Some(i) = self.files.iter().position(|f| f.path == path) {
            self.files[i].lufs = Some(ld.integrated_lufs);
            self.files[i].true_peak_dbfs = Some(ld.true_peak_dbfs);
        }
    }

    fn handle_done(&mut self) {
        for done in self.worker.poll() {
            match done {
                Done::Loaded(path, result) => {
                    self.pending.remove(&path);
                    match result {
                        Ok(l) if matches!(&l.source, Source::Whole(p) if p.sample_rate != self.device_rate()) => {
                            // Decoded for a device that has since been reopened at another rate.
                            self.request_load(&path);
                        }
                        Ok(l) => {
                            self.remember(l.clone());
                            let wanted = self.selected.and_then(|i| self.files.get(i)).map(|f| f.path == path).unwrap_or(false);
                            if wanted && self.current.as_ref().map(|c| c.path != path).unwrap_or(true) {
                                self.show_loaded(l);
                                if self.autoplay || std::mem::take(&mut self.play_on_load) {
                                    self.play(None);
                                }
                            }
                        }
                        Err(e) => self.set_status(format!("{}: {e}", path.display()), true),
                    }
                }
                Done::Analyzed(path, Err(e)) => self.set_status(format!("{}: {e}", path.display()), true),
                Done::Analyzed(path, Ok((peaks, loudness))) => {
                    self.record_loudness(&path, loudness);
                    if let Some(old) = self.cache.get(&path) {
                        let l = Arc::new(Loaded { path: old.path.clone(), source_rate: old.source_rate, channels: old.channels, duration_secs: old.duration_secs, source: old.source.clone(), peaks: Some(peaks), loudness });
                        self.cache.insert(path.clone(), l.clone());
                        if self.current.as_ref().map(|c| c.path == path).unwrap_or(false) {
                            self.current = Some(l);
                        }
                    }
                }
                Done::Exported(result) => match result {
                    Ok(r) => {
                        let mut gain = format!("{:+.2} dB", r.origin.measured.applied_gain_db);
                        for w in &r.warnings {
                            gain.push_str("  ·  ");
                            gain.push_str(w);
                        }
                        let shown = self.config.output_root().and_then(|root| r.output.strip_prefix(root).ok()).unwrap_or(&r.output);
                        self.set_status(format!("Exported {}  ·  {gain}", shown.display()), false);
                    }
                    Err(e) => self.set_status(format!("Export failed: {e}"), true),
                },
                Done::Scanned(reports) => {
                    self.scanning = false;
                    let total: usize = reports.iter().map(|(_, r)| r.seen).sum();
                    let errors: usize = reports.iter().map(|(_, r)| r.errors.len()).sum();
                    self.file_count = total;
                    self.set_status(if errors > 0 { format!("{errors} files could not be read") } else { String::new() }, errors > 0);
                    self.list_dirty = true;
                }
            }
        }
    }

    fn handle_keys(&mut self, ctx: &egui::Context) {
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        let n = self.files.len();
        let pressed = |k: Key| ctx.input(|i| i.key_pressed(k));
        let shift = ctx.input(|i| i.modifiers.shift);
        let go = |app: &mut App, i: usize| app.select(i, app.autoplay);
        if pressed(Key::ArrowDown) && n > 0 {
            go(self, self.selected.map(|i| (i + 1).min(n - 1)).unwrap_or(0));
        }
        if pressed(Key::ArrowUp) && n > 0 {
            go(self, self.selected.map(|i| i.saturating_sub(1)).unwrap_or(0));
        }
        if pressed(Key::PageDown) && n > 0 {
            go(self, self.selected.map(|i| (i + 10).min(n - 1)).unwrap_or(0));
        }
        if pressed(Key::PageUp) && n > 0 {
            go(self, self.selected.map(|i| i.saturating_sub(10)).unwrap_or(0));
        }
        if pressed(Key::Home) && n > 0 {
            go(self, 0);
        }
        if pressed(Key::End) && n > 0 {
            go(self, n - 1);
        }
        if pressed(Key::Space) {
            if self.is_playing() && !shift {
                self.stop();
            } else {
                self.play(None);
            }
        }
        if pressed(Key::Enter) {
            self.play(Some(0.0));
        }
        if pressed(Key::Escape) {
            self.stop();
        }
        if pressed(Key::F) {
            self.toggle_favorite();
        }
        if pressed(Key::L) {
            self.loop_on = !self.loop_on;
        }
        if pressed(Key::A) {
            self.autoplay = !self.autoplay;
        }
        if pressed(Key::E) {
            self.export();
        }
        if pressed(Key::Z) {
            self.wave.zoom_to_selection();
        }
        if pressed(Key::T) {
            self.trim_selection();
        }
        if pressed(Key::Delete) || pressed(Key::Backspace) {
            self.wave.selection = None;
            self.wave.fade_in = 0.0;
            self.wave.fade_out = 0.0;
        }
        if let Some(cur) = &self.current {
            let pos = self.player.as_ref().map(|p| p.position() as f64 / p.sample_rate as f64);
            if let Some(t) = pos {
                if pressed(Key::OpenBracket) {
                    let end = self.wave.selection.map(|(_, b)| b).filter(|b| *b > t).unwrap_or(cur.duration_secs);
                    self.wave.selection = Some((t, end));
                }
                if pressed(Key::CloseBracket) {
                    let start = self.wave.selection.map(|(a, _)| a).filter(|a| *a < t).unwrap_or(0.0);
                    self.wave.selection = Some((start, t));
                }
            }
        }
        let digits = [Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5, Key::Num6, Key::Num7, Key::Num8, Key::Num9];
        for (k, tag) in digits.iter().zip(self.config.tags.clone()) {
            if pressed(*k) {
                self.toggle_tag(&tag.name);
            }
        }
    }

    // ---- widgets ----

    fn chip(&self, ui: &mut egui::Ui, text: &str, color: Color32, on: bool, clickable: bool) -> egui::Response {
        let font = FontId::new(12.0, egui::FontFamily::Name(theme::MEDIUM.into()));
        let galley = ui.painter().layout_no_wrap(text.to_string(), font, Color32::WHITE);
        let pad = egui::vec2(8.0, 3.0);
        let size = galley.size() + pad * 2.0;
        let (rect, resp) = ui.allocate_exact_size(size, if clickable { Sense::click() } else { Sense::hover() });
        let hovered = clickable && resp.hovered();
        let (fill, stroke, fg) = if on {
            (color.gamma_multiply(if hovered { 0.45 } else { 0.32 }), Stroke::new(1.0, color.gamma_multiply(0.9)), color.gamma_multiply(1.6).to_opaque())
        } else {
            (if hovered { self.theme.select } else { Color32::TRANSPARENT }, Stroke::new(1.0, if hovered { self.theme.muted } else { self.theme.line }), self.theme.muted)
        };
        let p = ui.painter();
        p.rect_filled(rect, CornerRadius::same(10), fill);
        p.rect_stroke(rect, CornerRadius::same(10), stroke, StrokeKind::Inside);
        p.galley(rect.min + pad, galley, fg);
        resp
    }


    fn icon_button(ui: &mut egui::Ui, icon: &str, label: &str, on: bool) -> egui::Response {
        ui.add(egui::Button::new(theme::icon_text(icon, label, 14.0, egui::FontFamily::Proportional, Color32::PLACEHOLDER)).selected(on).min_size(egui::vec2(30.0, CONTROL_H)))
    }

    // ---- panels ----

    fn ui_top(&mut self, ui: &mut egui::Ui) {
        // One fixed row height, so every item centres on the same line whatever is placed
        // after it (egui centres each item in the row height known when it is placed).
        const ROW_H: f32 = TOP_BAR_H;
        ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), ROW_H), egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = theme::GAP;
            // The brand is one word: the mark stands on the letters' baseline and reaches
            // their ascender line, measured from the glyphs' own ink, and that block, not the
            // font box, is centred on the controls.
            let font = FontId::new(18.0, egui::FontFamily::Name(theme::SEMIBOLD.into()));
            let word = ui.fonts_mut(|f| f.layout_no_wrap("hark".into(), font.clone(), self.theme.accent));
            let (ink_top, ink_bottom) = word
                .rows
                .iter()
                .flat_map(|r| r.row.glyphs.iter().map(move |g| (r.pos.y + g.pos.y + g.uv_rect.offset.y, g.uv_rect.size.y)))
                .fold((f32::MAX, f32::MIN), |(t, b), (top, h)| (t.min(top), b.max(top + h)));
            let ink_h = (ink_bottom - ink_top).max(1.0);
            let [w, h] = self.mark.size();
            let mark_w = ink_h * w as f32 / h as f32;
            let gap = 5.0;
            let (rect, _) = ui.allocate_exact_size(egui::vec2(mark_w + gap + word.size().x, ROW_H), Sense::hover());
            let galley_top = rect.center().y - (ink_top + ink_bottom) / 2.0;
            let mark_rect = egui::Rect::from_min_size(egui::pos2(rect.left(), galley_top + ink_top), egui::vec2(mark_w, ink_h));
            egui::Image::new(&self.mark).tint(self.theme.accent2).paint_at(ui, mark_rect);
            ui.painter().galley(egui::pos2(mark_rect.right() + gap, galley_top), word, self.theme.accent);
            if let Some(action) = self.project_menu(ui) {
                self.apply_project_action(action);
            }
            let focused = ui.memory(|m| m.has_focus(self.search_id));
            let box_frame = egui::Frame::new()
                .fill(self.theme.surface)
                .stroke(Stroke::new(1.0, if focused { self.theme.accent } else { self.theme.line }))
                .corner_radius(CornerRadius::same(6))
                .inner_margin(Margin::symmetric(8, 0));
            ui.allocate_ui_with_layout(egui::vec2(300.0, CONTROL_H), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                box_frame.show(ui, |ui| {
                    ui.set_min_height(CONTROL_H - 2.0);
                    ui.spacing_mut().item_spacing.x = 6.0;
                    ui.label(theme::icon(icons::MAGNIFYING_GLASS).color(self.theme.muted));
                    let search = ui.add(egui::TextEdit::singleline(&mut self.search).id(self.search_id).frame(egui::Frame::NONE).hint_text("Search the library").desired_width(ui.available_width() - 20.0));
                    if search.changed() {
                        self.list_dirty = true;
                    }
                    if ui.input(|i| i.key_pressed(Key::Escape)) && search.has_focus() {
                        self.search.clear();
                        self.list_dirty = true;
                        search.surrender_focus();
                    }
                    if !self.search.is_empty() && ui.add(egui::Button::new(theme::icon(icons::X).color(self.theme.muted)).frame(false)).clicked() {
                        self.search.clear();
                        self.list_dirty = true;
                    }
                });
            });
            self.ui_filter(ui);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if Self::icon_button(ui, icons::GEAR, "", self.settings_open).on_hover_text("Settings").clicked() {
                    self.settings_open = !self.settings_open;
                }
            });
        });
    }

    fn add_root(&mut self) {
        let Some(path) = rfd::FileDialog::new().pick_folder() else { return };
        if self.config.roots().iter().any(|r| r.path == path) {
            return;
        }
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "root".into());
        self.config.roots_mut().push(hark_core::config::Root { name, path: path.clone() });
        if let Err(e) = self.config.save_target() {
            self.set_status(format!("Project: {e:#}"), true);
        }
        self.expanded.insert(path);
        self.rescan();
    }

    /// Takes a folder out of the open project (or the default state); the library rows stay,
    /// since another project may browse the same folder.
    fn remove_root(&mut self, path: &Path) {
        self.config.roots_mut().retain(|r| r.path != path);
        if let Err(e) = self.config.save_target() {
            self.set_status(format!("Project: {e:#}"), true);
        }
        if matches!(&self.location, Some(Location::Folder(d)) if d.starts_with(path)) {
            self.location = None;
        }
        self.list_dirty = true;
    }

    fn rescan(&mut self) {
        self.dir_children.clear();
        if self.config.roots().is_empty() {
            self.scanning = false;
            self.list_dirty = true;
            return;
        }
        self.scanning = true;
        self.worker.send(Job::Scan(self.config.roots().to_vec()));
    }

    fn ui_filter(&mut self, ui: &mut egui::Ui) {
        let n = self.filter.count();
        let label = if n == 0 { "Filter".to_string() } else { format!("Filter · {n}") };
        let button = Self::icon_button(ui, icons::FUNNEL, &label, n > 0);
        let mut changed = false;
        let popup = egui::Popup::menu(&button).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside);
        if std::env::var_os("HARK_OPEN_POPUP").map(|v| v == "filter").unwrap_or(false) {
            egui::Popup::open_id(ui.ctx(), popup.get_id());
        }
        let theme = self.theme.clone();
        // The list scrolls before it can run off the window; Clear stays below it.
        let max_h = ui.ctx().content_rect().height() - button.rect.bottom() - 90.0;
        popup.show(|ui| {
            theme::menu_items(ui);
            ui.set_width(190.0);
            egui::ScrollArea::vertical().max_height(max_h).auto_shrink([false, true]).show(ui, |ui| {
                changed |= theme::menu_check(ui, &mut self.filter.favorites, theme::icon_text(icons::STAR, "Favorites", 14.0, egui::FontFamily::Proportional, Color32::PLACEHOLDER)).changed();
                theme::menu_heading(ui, &theme, "Tags");
                for (k, t) in self.config.tags.clone().iter().enumerate() {
                    let mut on = self.filter.tags.contains(&t.name);
                    if theme::menu_check(ui, &mut on, RichText::new(&t.name).color(theme.color_of(k, Some(t)).gamma_multiply(1.3).to_opaque())).changed() {
                        toggle_in(&mut self.filter.tags, t.name.clone());
                        changed = true;
                    }
                }
                theme::menu_heading(ui, &theme, "File type");
                for (name, _) in FILE_TYPES {
                    let mut on = self.filter.types.contains(&name);
                    if theme::menu_check(ui, &mut on, name).changed() {
                        toggle_in(&mut self.filter.types, name);
                        changed = true;
                    }
                }
                theme::menu_heading(ui, &theme, "Channels");
                for c in ChannelClass::ALL {
                    let mut on = self.filter.channels.contains(&c);
                    if theme::menu_check(ui, &mut on, c.name()).changed() {
                        toggle_in(&mut self.filter.channels, c);
                        changed = true;
                    }
                }
            });
            ui.add_space(6.0);
            if ui.add_enabled(n > 0, egui::Button::new(theme::icon_text(icons::FUNNEL_X, "Clear", 14.0, egui::FontFamily::Proportional, Color32::PLACEHOLDER))).clicked() {
                self.filter.clear();
                changed = true;
            }
        });
        if changed {
            self.list_dirty = true;
        }
    }

    fn ui_tree(&mut self, ui: &mut egui::Ui) {
        let bar = egui::Frame::new().inner_margin(Margin { left: 8, right: 8, top: 8, bottom: 6 });
        egui::Panel::bottom("library-actions").resizable(false).frame(bar).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let w = (ui.available_width() - 6.0) / 2.0;
                if ui.add(theme::tool_button_widget(icons::FOLDER_PLUS, "Add folder").min_size(egui::vec2(w, 30.0))).on_hover_text("Add a library folder").clicked() {
                    self.add_root();
                }
                let can = !self.scanning && !self.config.roots().is_empty();
                if ui.add_enabled(can, theme::tool_button_widget(icons::ARROWS_CLOCKWISE, "Rescan").min_size(egui::vec2(w, 30.0))).on_hover_text("Walk the library folders again").clicked() {
                    self.rescan();
                }
            });
        });
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            let selected = self.location == Some(Location::Favorites);
            let count = if self.fav_count > 0 { Some(self.fav_count.to_string()) } else { None };
            let (row, _) = self.tree_row(ui, 0, None, (icons::STAR, theme::ICONS_FILL), self.theme.accent, "Favorites", selected, count);
            if row.clicked() {
                self.location = Some(Location::Favorites);
                self.search.clear();
                self.list_dirty = true;
            }
            ui.add_space(6.0);
            if self.config.roots().is_empty() {
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.add_space(8.0);
                    ui.weak(if self.config.active.is_some() { "No folders in this project yet. Add one below." } else { "No library folders yet. Add one below." });
                });
                return;
            }
            for r in self.config.roots().to_vec() {
                self.ui_tree_node(ui, &r.path, &r.name, 0);
            }
        });
    }

    /// One row of the left bar: an optional caret (`Some(expanded)`), an icon, a label and an
    /// optional trailing note. Returns the row's response and the caret's.
    #[allow(clippy::too_many_arguments)]
    fn tree_row(&self, ui: &mut egui::Ui, depth: usize, caret: Option<bool>, icon: (&str, &str), icon_color: Color32, label: &str, selected: bool, trailing: Option<String>) -> (egui::Response, egui::Response) {
        let row_h = theme::ROW_H;
        let width = ui.available_width();
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, row_h), Sense::click());
        let caret_rect = egui::Rect::from_min_size(rect.min + egui::vec2(6.0 + depth as f32 * 16.0, 0.0), egui::vec2(18.0, row_h));
        let caret_resp = ui.interact(caret_rect, resp.id.with("caret"), Sense::click());
        let fill = if selected {
            self.theme.accent2_soft
        } else if resp.hovered() {
            self.theme.overlay
        } else {
            Color32::TRANSPARENT
        };
        let p = ui.painter();
        p.rect_filled(rect.shrink2(egui::vec2(4.0, 1.0)), CornerRadius::same(5), fill);
        let text_color = if selected { self.theme.text } else { self.theme.text.gamma_multiply(0.85) };
        let font = FontId::new(14.0, egui::FontFamily::Proportional);
        let icon_font = FontId::new(14.0, egui::FontFamily::Name(theme::ICONS.into()));
        let x = caret_rect.left();
        if let Some(expanded) = caret {
            let glyph = if expanded { icons::CARET_DOWN } else { icons::CARET_RIGHT };
            p.text(egui::pos2(x + 2.0, rect.center().y), egui::Align2::LEFT_CENTER, glyph, icon_font, self.theme.muted);
        }
        let (glyph, family) = icon;
        p.text(egui::pos2(x + 20.0, rect.center().y), egui::Align2::LEFT_CENTER, glyph, FontId::new(14.0, egui::FontFamily::Name(family.into())), if selected { icon_color } else { self.theme.muted });
        p.text(egui::pos2(x + 40.0, rect.center().y), egui::Align2::LEFT_CENTER, label, font, text_color);
        if let Some(t) = trailing {
            p.text(egui::pos2(rect.right() - 10.0, rect.center().y), egui::Align2::RIGHT_CENTER, t, FontId::new(12.0, egui::FontFamily::Proportional), self.theme.muted);
        }
        (resp, caret_resp)
    }

    fn ui_tree_node(&mut self, ui: &mut egui::Ui, dir: &Path, label: &str, depth: usize) {
        let children = self.children_of(dir);
        let expanded = self.expanded.contains(dir);
        let selected = self.location.as_ref() == Some(&Location::Folder(dir.to_path_buf()));
        let folder = if expanded && !children.is_empty() { icons::FOLDER_OPEN } else { icons::FOLDER };
        let (resp, caret_resp) = self.tree_row(ui, depth, (!children.is_empty()).then_some(expanded), (folder, theme::ICONS), self.theme.accent, label, selected, None);

        if caret_resp.clicked() && !children.is_empty() {
            if expanded {
                self.expanded.remove(dir);
            } else {
                self.expanded.insert(dir.to_path_buf());
            }
        } else if resp.clicked() {
            self.location = Some(Location::Folder(dir.to_path_buf()));
            self.search.clear();
            self.list_dirty = true;
            if !expanded && !children.is_empty() {
                self.expanded.insert(dir.to_path_buf());
            }
        }
        if resp.double_clicked() && !children.is_empty() {
            if self.expanded.contains(dir) {
                self.expanded.remove(dir);
            } else {
                self.expanded.insert(dir.to_path_buf());
            }
        }
        let mut remove = false;
        let mut show = false;
        resp.context_menu(|ui| {
            theme::menu_items(ui);
            if depth == 0 && ui.button(theme::icon_text(icons::X, "Remove folder from the project", 14.0, egui::FontFamily::Proportional, Color32::PLACEHOLDER)).clicked() {
                remove = true;
                ui.close();
            }
            if ui.button(theme::icon_text(icons::FOLDER_OPEN, "Show in folder", 14.0, egui::FontFamily::Proportional, Color32::PLACEHOLDER)).clicked() {
                show = true;
                ui.close();
            }
        });
        if remove {
            self.remove_root(dir);
            return;
        }
        if show {
            if let Err(e) = reveal(dir) {
                self.set_status(format!("Show in folder: {e}"), true);
            }
        }
        if self.expanded.contains(dir) {
            for child in children {
                let name = child.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                self.ui_tree_node(ui, &child, &name, depth + 1);
            }
        }
    }

    fn ui_list(&mut self, ui: &mut egui::Ui) {
        let n = self.files.len();
        let heading = if !self.search.trim().is_empty() {
            if n >= SEARCH_LIMIT { format!("First {n} results") } else { format!("{n} results") }
        } else {
            match &self.location {
                Some(Location::Folder(d)) => {
                    let shown = self.config.root_of(d).and_then(|r| d.strip_prefix(&r.path).ok()).map(|rel| if rel.as_os_str().is_empty() { String::new() } else { format!("  /  {}", rel.display()) });
                    match (self.config.root_of(d), shown) {
                        (Some(r), Some(rel)) => format!("{}{rel}   ·   {n} files", r.name),
                        _ => format!("{}   ·   {n} files", d.display()),
                    }
                }
                Some(Location::Favorites) => format!("Favorites of {}   ·   {n} files", self.config.project_name()),
                None => "Pick a folder".to_string(),
            }
        };
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.label(RichText::new(heading).color(self.theme.muted));
        });
        ui.add_space(6.0);
        let row_h = theme::ROW_H;
        let mut clicked: Option<usize> = None;
        let mut double: Option<usize> = None;
        let mut context: Option<usize> = None;
        let mut fav_toggle: Option<usize> = None;
        let mut action: Option<(usize, FileAction)> = None;
        let defs = self.config.tags.clone();
        let mut builder = TableBuilder::new(ui)
            .striped(true)
            .sense(Sense::click())
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::exact(26.0))
            .column(Column::remainder().at_least(220.0).clip(true))
            .column(Column::initial(200.0).at_least(60.0).clip(true))
            .column(Column::exact(74.0))
            .column(Column::exact(30.0))
            .column(Column::exact(56.0))
            .column(Column::exact(36.0))
            .column(Column::exact(80.0))
            .column(Column::exact(60.0));
        if self.scroll_to_selected {
            if let Some(i) = self.selected {
                builder = builder.scroll_to_row(i, None);
            }
            self.scroll_to_selected = false;
        }
        let theme = self.theme.clone();
        let tag_colors: HashMap<String, Color32> = self.config.tags.iter().enumerate().map(|(i, t)| (t.name.clone(), self.theme.color_of(i, Some(t)))).collect();
        let unknown_color = self.theme.tag_color(self.theme.tag_colors.len() - 1);
        let files = &self.files;
        let selected = self.selected;
        let chip = |ui: &mut egui::Ui, text: &str, color: Color32| {
            let font = FontId::new(11.0, egui::FontFamily::Name(theme::MEDIUM.into()));
            let galley = ui.painter().layout_no_wrap(text.to_string(), font, Color32::WHITE);
            let pad = egui::vec2(7.0, 2.0);
            let (rect, _) = ui.allocate_exact_size(galley.size() + pad * 2.0, Sense::hover());
            ui.painter().rect_filled(rect, CornerRadius::same(9), color.gamma_multiply(0.28));
            ui.painter().galley(rect.min + pad, galley, color.gamma_multiply(1.5).to_opaque());
        };
        builder
            .header(24.0, |mut h| {
                for title in ["", "Name", "Tags", "Length", "Ch", "Rate", "Bits", "Codec", "LUFS"] {
                    h.col(|ui| {
                        ui.label(RichText::new(title).color(theme.muted).size(12.0));
                    });
                }
            })
            .body(|body| {
                body.rows(row_h, n, |mut row| {
                    let i = row.index();
                    let f = &files[i];
                    row.set_selected(selected == Some(i));
                    let (_, star) = row.col(|ui| {
                        let hot = ui.rect_contains_pointer(ui.max_rect());
                        let color = match (f.favorite, hot) {
                            (true, true) => theme.accent.gamma_multiply(1.4).to_opaque(),
                            (true, false) | (false, true) => theme.accent,
                            (false, false) => theme.muted.gamma_multiply(0.7),
                        };
                        let icon = if f.favorite || hot { theme::icon_fill(icons::STAR) } else { theme::icon(icons::STAR) };
                        ui.label(icon.color(color).size(15.0));
                    });
                    if star.clicked() {
                        fav_toggle = Some(i);
                    }
                    row.col(|ui| {
                        let text = if f.error.is_some() { RichText::new(&f.name).color(theme.err) } else { RichText::new(&f.name) };
                        ui.label(text);
                    });
                    row.col(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        for t in &f.tags {
                            chip(ui, t, tag_colors.get(t).copied().unwrap_or(unknown_color));
                        }
                    });
                    let info = f.info.as_ref();
                    let weak = |s: String| RichText::new(s).color(theme.muted);
                    row.col(|ui| {
                        ui.label(info.map(|x| waveform::fmt_time(x.duration_secs)).unwrap_or_default());
                    });
                    row.col(|ui| {
                        ui.label(weak(info.map(|x| x.channels.to_string()).unwrap_or_default()));
                    });
                    row.col(|ui| {
                        ui.label(weak(info.map(|x| format!("{:.1}k", x.sample_rate as f64 / 1000.0)).unwrap_or_default()));
                    });
                    row.col(|ui| {
                        ui.label(weak(info.and_then(|x| x.bits_per_sample).map(|b| b.to_string()).unwrap_or_default()));
                    });
                    row.col(|ui| {
                        ui.label(weak(info.map(|x| x.codec.clone()).unwrap_or_default()));
                    });
                    row.col(|ui| {
                        ui.label(weak(f.lufs.map(|l| format!("{l:.1}")).unwrap_or_default()));
                    });
                    let resp = row.response();
                    if fav_toggle == Some(i) {
                    } else if resp.double_clicked() {
                        double = Some(i);
                    } else if resp.clicked() {
                        clicked = Some(i);
                    } else if resp.secondary_clicked() {
                        context = Some(i);
                    }
                    egui::Popup::context_menu(&resp).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
                        if let Some(a) = file_menu(ui, f, &defs, &theme) {
                            action = Some((i, a));
                        }
                    });
                });
            });
        if let Some(i) = fav_toggle {
            self.toggle_favorite_of(i);
        }
        if let Some(i) = double {
            self.select(i, true);
        } else if let Some(i) = clicked {
            self.select(i, self.autoplay);
        } else if let Some(i) = context {
            if self.selected != Some(i) {
                self.select(i, false);
            }
        }
        if let Some((i, a)) = action {
            self.apply_file_action(i, a);
        }
    }

    fn apply_file_action(&mut self, i: usize, action: FileAction) {
        let Some(f) = self.files.get(i) else { return };
        match action {
            FileAction::Favorite => self.toggle_favorite_of(i),
            FileAction::Tag(t) => self.toggle_tag_of(i, &t),
            FileAction::Reveal => {
                if let Err(e) = reveal(&f.path) {
                    self.set_status(format!("Show in folder: {e}"), true);
                }
            }
            FileAction::CopyPath => {
                let p = f.path.display().to_string();
                self.set_status(format!("Copied {p}"), false);
            }
        }
    }

    /// Two cards side by side: the file with its waveform and transport, and the export.
    fn ui_bottom(&mut self, ui: &mut egui::Ui) {
        let card = egui::Frame::new().fill(self.theme.surface).corner_radius(CornerRadius::same(8)).inner_margin(Margin::same(theme::CARD_PAD));
        egui::Panel::right("export").resizable(false).exact_size(EXPORT_CARD_W).show_separator_line(false).frame(egui::Frame::new().inner_margin(Margin { left: theme::PAD, right: 0, top: 0, bottom: 0 })).show(ui, |ui| {
            card.show(ui, |ui| {
                ui.set_min_height(ui.available_height());
                self.ui_export_card(ui);
            });
        });
        egui::CentralPanel::default().frame(egui::Frame::new().inner_margin(Margin::ZERO)).show(ui, |ui| {
            card.show(ui, |ui| {
                ui.set_min_height(ui.available_height());
                self.ui_wave_column(ui);
            });
        });
    }

    fn ui_wave_column(&mut self, ui: &mut egui::Ui) {
        let playing = self.is_playing();
        let playhead = match (&self.player, &self.current) {
            (Some(p), Some(_)) if playing => Some(p.position() as f64 / p.sample_rate as f64),
            _ => None,
        };
        let duration = self.current.as_ref().map(|c| c.duration_secs).unwrap_or(0.0);

        // The file's tag chips and the tag button sit at the right of the header when they fit
        // beside the name, else on a row of their own.
        let carried: Vec<String> = self.selected.map(|i| self.files[i].tags.clone()).unwrap_or_default();
        let chips_w = carried.iter().map(|t| self.chip_width(ui, t) + 6.0).sum::<f32>() + 80.0 + theme::GAP;
        let mut chips_below = false;
        ui.horizontal(|ui| {
            if let Some(c) = self.current.clone() {
                let name = c.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                ui.label(theme::semibold(name).size(15.0));
                let mut meta = format!("{} Hz  ·  {} ch  ·  {}", c.source_rate, c.channels, waveform::fmt_time(c.duration_secs));
                let record = self.selected.and_then(|i| self.files.get(i)).filter(|f| f.path == c.path);
                let loudness = c.loudness.map(|l| (l.integrated_lufs, l.true_peak_dbfs, l.momentary)).or_else(|| record.and_then(|f| Some((f.lufs?, f.true_peak_dbfs?, false))));
                if let Some((lufs, tp, momentary)) = loudness {
                    meta.push_str(&format!("  ·  {lufs:.1} LUFS{}  ·  {tp:.1} dBTP", if momentary { " (M)" } else { "" }));
                }
                if c.source.is_streamed() {
                    meta.push_str("  ·  streaming");
                }
                ui.add(egui::Label::new(RichText::new(meta).color(self.theme.muted)).truncate());
                if c.peaks.is_none() {
                    ui.spinner();
                    ui.label(RichText::new("Analysing").color(self.theme.muted));
                }
                if self.selected.is_some() {
                    if ui.available_width() >= chips_w {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| self.tag_chips(ui, true));
                    } else {
                        chips_below = true;
                    }
                }
            } else if let Some(i) = self.selected {
                if self.pending.contains(&self.files[i].path) {
                    ui.spinner();
                    ui.label(RichText::new("Loading").color(self.theme.muted));
                }
            } else {
                ui.label(RichText::new("No file").color(self.theme.muted));
            }
        });
        if chips_below {
            ui.add_space(2.0);
            ui.horizontal_wrapped(|ui| self.tag_chips(ui, false));
        }

        ui.add_space(4.0);
        // The transport row wraps at narrow widths; the waveform leaves it the height it took on
        // the last frame.
        let wave_h = (ui.available_height() - self.transport_h - ui.spacing().item_spacing.y).max(80.0);
        let peaks = self.current.as_ref().and_then(|c| c.peaks.clone());
        let samples = self.current.as_ref().and_then(|c| match &c.source {
            Source::Whole(p) => Some(p.clone()),
            Source::File { .. } => None,
        });
        let colors = waveform::WaveColors {
            peak: self.theme.accent.gamma_multiply(0.55),
            rms: self.theme.accent.gamma_multiply(0.95),
            selection: self.theme.accent2,
            fade: self.theme.accent2.gamma_multiply(1.3).to_opaque(),
            playhead: self.theme.err,
            axis: self.theme.line,
        };
        // The gain fader stands beside the waveform and scales it, so what you hear is what
        // you see.
        const FADER_W: f32 = 36.0;
        self.wave.gain = hark_core::pcm::db_to_gain(self.gain_db as f64) as f32;
        let mut event = WaveEvent::None;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let wave_w = ui.available_width() - FADER_W - 8.0;
            ui.allocate_ui_with_layout(egui::vec2(wave_w, wave_h), egui::Layout::top_down(egui::Align::Min), |ui| {
                event = waveform::show(ui, &mut self.wave, peaks.as_deref(), samples.as_deref(), duration, playhead, wave_h, &colors);
            });
            // The readout sits level with the fade lane and the rail with the wave itself.
            ui.allocate_ui_with_layout(egui::vec2(FADER_W, wave_h), egui::Layout::top_down(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                let readout = ui.allocate_response(egui::vec2(FADER_W, waveform::LANE_H), egui::Sense::hover()).on_hover_text("dB");
                ui.spacing_mut().slider_width = wave_h - waveform::LANE_H;
                let fader = ui.add(egui::Slider::new(&mut self.gain_db, -36.0..=12.0).vertical().show_value(false).trailing_fill(false)).on_hover_text("Audition gain; double-click resets");
                ui.painter().text(egui::pos2(fader.rect.center().x, readout.rect.center().y), egui::Align2::CENTER_CENTER, format!("{:+.0}", self.gain_db), FontId::proportional(11.0), self.theme.muted);
                if fader.double_clicked() {
                    self.gain_db = 0.0;
                }
                if fader.changed() || fader.double_clicked() {
                    if let Some(p) = &self.player {
                        p.set_gain(hark_core::pcm::db_to_gain(self.gain_db as f64));
                    }
                }
            });
        });
        if let WaveEvent::Seek(t) = event {
            self.play(Some(t));
        }
        let row = ui.scope(|ui| self.ui_transport(ui, playing, playhead, duration)).response.rect.height();
        if (row - self.transport_h).abs() > 0.5 {
            self.transport_h = row;
            ui.ctx().request_repaint();
        }
    }

    fn chip_width(&self, ui: &egui::Ui, text: &str) -> f32 {
        let font = FontId::new(12.0, egui::FontFamily::Name(theme::MEDIUM.into()));
        ui.painter().layout_no_wrap(text.to_string(), font, Color32::WHITE).size().x + 16.0
    }

    /// The selected file's own tags as chips, and a tag button opening the file menu;
    /// `reversed` keeps the reading order inside a right-to-left layout.
    fn tag_chips(&mut self, ui: &mut egui::Ui, reversed: bool) {
        let Some(i) = self.selected else { return };
        ui.spacing_mut().item_spacing.x = 6.0;
        let defs = self.config.tags.clone();
        let file = self.files[i].clone();
        let mut action = None;
        let menu_button = |ui: &mut egui::Ui, action: &mut Option<FileAction>| {
            let b = theme::tool_button(ui, icons::TAG, "Tags").on_hover_text("Favorite, tags and more; also on right-click of a row");
            egui::Popup::menu(&b).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
                *action = file_menu(ui, &file, &defs, &self.theme);
            });
        };
        let chips: Vec<(usize, &str)> = file.tags.iter().map(|t| (defs.iter().position(|d| &d.name == t).unwrap_or(defs.len()), t.as_str())).collect();
        if reversed {
            menu_button(ui, &mut action);
            for (k, t) in chips.iter().rev() {
                self.chip(ui, t, self.theme.color_of(*k, defs.get(*k)), true, false);
            }
        } else {
            for (k, t) in &chips {
                self.chip(ui, t, self.theme.color_of(*k, defs.get(*k)), true, false);
            }
            menu_button(ui, &mut action);
        }
        if let Some(a) = action {
            self.apply_file_action(i, a);
        }
    }

    fn ui_transport(&mut self, ui: &mut egui::Ui, playing: bool, playhead: Option<f64>, duration: f64) {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().interact_size = egui::vec2(CONTROL_H, CONTROL_H);
            ui.spacing_mut().button_padding = egui::vec2(8.0, 5.0);
            ui.spacing_mut().item_spacing.x = 6.0;
            let icon_btn = |ui: &mut egui::Ui, icon: &str, on: bool| ui.add(egui::Button::new(theme::icon(icon).size(15.0)).selected(on).min_size(egui::vec2(CONTROL_H + 4.0, CONTROL_H)));
            let group = |ui: &mut egui::Ui| {
                ui.add_space(theme::GAP - ui.spacing().item_spacing.x);
                ui.separator();
                ui.add_space(theme::GAP - ui.spacing().item_spacing.x);
            };
            let play = if playing { icons::STOP } else { icons::PLAY };
            if icon_btn(ui, play, false).on_hover_text("Play / stop (Space)").clicked() {
                if playing {
                    self.stop();
                } else {
                    self.play(None);
                }
            }
            if icon_btn(ui, icons::REPEAT, self.loop_on).on_hover_text("Loop (L)").clicked() {
                self.loop_on = !self.loop_on;
            }
            if ui.add(egui::Button::new("Auto").selected(self.autoplay).min_size(egui::vec2(0.0, CONTROL_H))).on_hover_text("Autoplay on selection (A)").clicked() {
                self.autoplay = !self.autoplay;
            }
            let t = playhead.unwrap_or(0.0);
            ui.add_sized(egui::vec2(52.0, CONTROL_H), egui::Label::new(RichText::new(waveform::fmt_time(t)).monospace().color(if playing { self.theme.text } else { self.theme.muted })));
            group(ui);
            ui.label(RichText::new("Sel").color(self.theme.muted));
            let mut sel = self.wave.selection.unwrap_or((0.0, 0.0));
            let had = self.wave.selection.is_some();
            let a = ui.add(secs_field(&mut sel.0, duration));
            let b = ui.add(secs_field(&mut sel.1, duration));
            if (a.changed() || b.changed()) && (had || sel.1 > sel.0) {
                self.wave.selection = if sel.1 > sel.0 { Some(sel) } else { None };
            }
            if icon_btn(ui, icons::X, false).on_hover_text("Clear selection (Del)").clicked() {
                self.wave.selection = None;
            }
            if icon_btn(ui, icons::MAGNIFYING_GLASS_PLUS, false).on_hover_text("Zoom to selection (Z)").clicked() {
                self.wave.zoom_to_selection();
            }
            if icon_btn(ui, icons::ARROWS_IN_LINE_HORIZONTAL, false).on_hover_text(format!("Trim the selection to the sound (T): what sits {} dB under its peak goes", hark_core::pcm::TRIM_BELOW_PEAK_DB)).clicked() {
                self.trim_selection();
            }
            group(ui);
            ui.label(RichText::new("Fade").color(self.theme.muted));
            ui.add(secs_field(&mut self.wave.fade_in, duration));
            ui.add(secs_field(&mut self.wave.fade_out, duration));
            egui::ComboBox::from_id_salt("fade-shape")
                .width(64.0)
                .selected_text(match self.wave.fade_shape {
                    hark_core::pcm::FadeShape::EqualPower => "Eq",
                    hark_core::pcm::FadeShape::Linear => "Lin",
                    hark_core::pcm::FadeShape::Logarithmic => "Log",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.wave.fade_shape, hark_core::pcm::FadeShape::EqualPower, "Equal power");
                    ui.selectable_value(&mut self.wave.fade_shape, hark_core::pcm::FadeShape::Linear, "Linear");
                    ui.selectable_value(&mut self.wave.fade_shape, hark_core::pcm::FadeShape::Logarithmic, "Logarithmic");
                });
            self.wave.clamp_fades(duration);
        });
    }

    /// Inside the export card: the preset and its values, where the file goes, the button.
    fn ui_export_card(&mut self, ui: &mut egui::Ui) {
        use preset_ui::CONTROL_H;
        let theme = self.theme.clone();
        ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
        ui.spacing_mut().interact_size = egui::vec2(CONTROL_H, CONTROL_H);
        ui.spacing_mut().button_padding = egui::vec2(8.0, 4.0);
        let small = |text: String, color: Color32| egui::Label::new(RichText::new(text).color(color).size(12.5)).truncate();
        let key_w = 52.0;
        let value_w = ui.available_width() - key_w - 6.0;
        let key = |ui: &mut egui::Ui, text: &str| {
            ui.add_sized(egui::vec2(key_w, CONTROL_H), egui::Label::new(RichText::new(text).color(theme.muted).size(12.5)));
        };
        let rule = |ui: &mut egui::Ui| {
            ui.add_space(2.0);
            ui.separator();
            ui.add_space(2.0);
        };

        ui.horizontal(|ui| {
            ui.label(theme::semibold("Export").size(15.0));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| self.preset_picker(ui));
        });
        rule(ui);

        let mut edited = false;
        let p = &mut self.preset;
        egui::Grid::new("export-preset").num_columns(2).spacing([6.0, 6.0]).show(ui, |ui| {
            key(ui, "Format");
            ui.horizontal(|ui| {
                edited |= preset_ui::format(ui, "card", p);
                edited |= preset_ui::bits(ui, "card", p);
                edited |= preset_ui::dither(ui, p);
            });
            ui.end_row();
            key(ui, "Rate");
            edited |= preset_ui::sample_rate(ui, "card", p);
            ui.end_row();
            key(ui, "Channels");
            edited |= preset_ui::channels(ui, "card", p);
            ui.end_row();
            key(ui, "Level");
            ui.horizontal(|ui| edited |= preset_ui::normalize(ui, "card", p));
            ui.end_row();
        });
        if edited {
            self.preset_name = None;
        }
        rule(ui);

        let req = self.export_request();
        let file = self.selected.map(|i| self.files[i].clone());
        // The target row: the template's folder, the name field, the extension.
        let (dir, ext, outcome) = match (&req, &file) {
            (Some(Ok(r)), Some(f)) => {
                let templated = PathBuf::from(hark_core::export::render_template(&r.preset.path_template, r));
                let dir = templated.parent().filter(|d| !d.as_os_str().is_empty()).map(|d| format!("{}/", d.display())).unwrap_or_default();
                (dir, format!(".{}", r.effective_preset().format.extension()), self.export_outcome(r, f))
            }
            _ => (String::new(), String::new(), "…".into()),
        };
        let stem = file.as_ref().and_then(|f| f.path.file_stem()).map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "File name".into());
        let out = self.config.output_root().map(tilde);
        let have = file.is_some();
        egui::Grid::new("export-target").num_columns(2).spacing([6.0, 6.0]).show(ui, |ui| {
            key(ui, "Folder");
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                let sub_w = 120.0;
                let root_w = value_w - sub_w - 4.0;
                let folder = egui::Button::new(theme::icon_text(icons::FOLDER, out.as_deref().unwrap_or("Choose…"), 12.5, egui::FontFamily::Proportional, Color32::PLACEHOLDER)).min_size(egui::vec2(root_w, CONTROL_H)).truncate();
                let folder = ui.add_sized(egui::vec2(root_w, CONTROL_H), folder);
                let folder = match &out {
                    Some(o) => folder.on_hover_text(format!("{o}\nThe output folder")),
                    None => folder.on_hover_text("The output folder"),
                };
                if folder.clicked() {
                    if let Some(path) = rfd::FileDialog::new().pick_folder() {
                        if let Err(e) = self.config.set_output_root(Some(path)) {
                            self.set_status(format!("Output folder: {e:#}"), true);
                        }
                    }
                }
                let root = self.config.output_root().map(|p| p.to_path_buf());
                let current = self.config.export_folder().map(|p| p.to_path_buf());
                let shown = current.as_ref().map(|p| format!("{}/", p.display())).unwrap_or_else(|| "/".into());
                let mut pick: Option<Option<PathBuf>> = None;
                let mut browse = false;
                ui.add_enabled_ui(root.is_some(), |ui| {
                    egui::ComboBox::from_id_salt("export-subfolder").width(sub_w).truncate().selected_text(RichText::new(shown).size(12.5)).show_ui(ui, |ui| {
                        if ui.selectable_label(current.is_none(), "/").clicked() {
                            pick = Some(None);
                        }
                        for sub in root.as_deref().map(subfolders).unwrap_or_default() {
                            if ui.selectable_label(current.as_deref() == Some(&sub), format!("{}/", sub.display())).clicked() {
                                pick = Some(Some(sub));
                            }
                        }
                        ui.separator();
                        if ui.selectable_label(false, theme::icon_text(icons::FOLDER_OPEN, "Browse…", 13.0, egui::FontFamily::Proportional, Color32::PLACEHOLDER)).on_hover_text("Pick or create a folder under the output folder").clicked() {
                            browse = true;
                        }
                    });
                }).response.on_hover_text("The folder under the output folder this export goes to");
                if browse {
                    if let Some(root) = &root {
                        if let Some(path) = rfd::FileDialog::new().set_directory(root).pick_folder() {
                            match path.strip_prefix(root) {
                                Ok(rel) => pick = Some((!rel.as_os_str().is_empty()).then(|| rel.to_path_buf())),
                                Err(_) => self.set_status(format!("{} is not under the output folder", path.display()), true),
                            }
                        }
                    }
                }
                if let Some(folder) = pick {
                    if let Err(e) = self.config.set_export_folder(folder) {
                        self.set_status(format!("Export folder: {e:#}"), true);
                    }
                }
            });
            ui.end_row();
            key(ui, "Name");
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                let ext_w = 34.0;
                let mut rest = value_w - ext_w - 2.0;
                if !dir.is_empty() {
                    let dir_w = ui.fonts_mut(|f| f.layout_no_wrap(dir.clone(), FontId::proportional(12.5), theme.muted).size().x).min(rest * 0.4);
                    ui.add_sized(egui::vec2(dir_w, CONTROL_H), egui::Label::new(RichText::new(&dir).color(theme.muted).size(12.5)).truncate()).on_hover_text("From the preset's path template");
                    rest -= dir_w + 2.0;
                }
                ui.add_enabled(have, egui::TextEdit::singleline(&mut self.export_name).hint_text(&stem).desired_width(rest.max(50.0)).vertical_align(egui::Align::Center).min_size(egui::vec2(0.0, CONTROL_H)));
                ui.add_sized(egui::vec2(ext_w, CONTROL_H), egui::Label::new(RichText::new(&ext).color(theme.muted).size(12.5)));
            });
            ui.end_row();
            key(ui, "Writes");
            ui.horizontal(|ui| {
                ui.add_enabled(have, small(outcome, theme.text));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_enabled(have, theme::Check::new(&mut self.overwrite, RichText::new("Overwrite").size(12.5)));
                });
            });
            ui.end_row();
        });
        if let Some(Err(e)) = &req {
            ui.add(small(e.clone(), theme.warn));
        }

        ui.add_space(10.0);
        let can = matches!(req, Some(Ok(_)));
        let rect = egui::Rect::from_min_size(ui.cursor().min, egui::vec2(ui.available_width(), 40.0));
        let hot = can && ui.rect_contains_pointer(rect);
        let fill = match (can, hot) {
            (true, true) => theme.accent.gamma_multiply(1.15).to_opaque(),
            (true, false) => theme.accent,
            (false, _) => theme.overlay,
        };
        let button = egui::Button::new(theme::icon_text(icons::EXPORT, "Export", 15.0, egui::FontFamily::Name(theme::SEMIBOLD.into()), if can { theme.on_accent() } else { theme.muted }))
            .fill(fill)
            .stroke(Stroke::NONE)
            .corner_radius(CornerRadius::same(6))
            .min_size(rect.size());
        ui.allocate_ui_with_layout(rect.size(), egui::Layout::centered_and_justified(egui::Direction::LeftToRight), |ui| {
            if ui.add_enabled(can, button).on_hover_text("E").clicked() {
                self.export();
            }
        });
        let h = ui.min_rect().height();
        if (h - self.export_card_h).abs() > 0.5 {
            self.export_card_h = h;
            ui.ctx().request_repaint();
        }
    }

    /// The preset combo: the saved presets, "custom" once the values were edited, and a way to
    /// keep the edited values as a new preset.
    fn preset_picker(&mut self, ui: &mut egui::Ui) {
        let label = self.preset_name.clone().unwrap_or_else(|| "Custom".into());
        let mut pick: Option<String> = None;
        let mut save_as = false;
        egui::ComboBox::from_id_salt("preset").width(150.0).selected_text(label).show_ui(ui, |ui| {
            for name in self.config.presets.keys() {
                if ui.selectable_label(self.preset_name.as_deref() == Some(name), name).clicked() {
                    pick = Some(name.clone());
                }
            }
            if self.preset_name.is_none() {
                let _ = ui.selectable_label(true, "Custom");
                ui.separator();
                if ui.selectable_label(false, theme::icon_text(icons::FLOPPY_DISK, "Save as preset…", 14.0, egui::FontFamily::Proportional, Color32::PLACEHOLDER)).clicked() {
                    save_as = true;
                }
            }
        });
        if let Some(name) = pick {
            self.use_preset(&name);
        }
        if save_as {
            let name = self.config.free_preset_name("custom");
            self.config.presets.insert(name.clone(), self.preset.clone());
            let _ = self.config.save();
            self.preset_name = Some(name.clone());
            self.preset_edit = name;
            self.settings_page = settings::Page::Presets;
            self.settings_open = true;
        }
    }

    fn use_preset(&mut self, name: &str) {
        if let Some(p) = self.config.preset(name) {
            self.preset = p.clone();
            self.preset_name = Some(name.to_string());
        }
    }

    /// The project switcher: the default state, the known project files, and ways to open or
    /// start one. `width` of zero lets the button size itself.
    fn project_menu(&mut self, ui: &mut egui::Ui) -> Option<ProjectAction> {
        let mut action = None;
        let mut label = theme::icon_text(icons::CUBE, &self.config.project_name(), 14.0, egui::FontFamily::Proportional, Color32::PLACEHOLDER);
        label.append(icons::CARET_DOWN, 7.0, egui::text::TextFormat { font_id: FontId::new(12.0, egui::FontFamily::Name(theme::ICONS.into())), color: Color32::PLACEHOLDER, valign: egui::Align::Center, ..Default::default() });
        let button = ui.add(egui::Button::new(label).min_size(egui::vec2(30.0, CONTROL_H)));
        egui::Popup::menu(&button).close_behavior(egui::PopupCloseBehavior::CloseOnClick).width(220.0).show(|ui| {
            theme::menu_items(ui);
            if ui.selectable_label(self.config.active.is_none(), "Default").clicked() {
                action = Some(ProjectAction::Default);
            }
            for path in self.config.project_files.clone() {
                let open = self.config.project.as_deref() == Some(&path);
                let name = hark_core::project::name_of_path(&path);
                let missing = !path.is_file();
                let text = if missing { RichText::new(format!("{name}  (missing)")).color(self.theme.muted) } else { RichText::new(name) };
                ui.horizontal(|ui| {
                    if ui.add_enabled(!missing, egui::Button::selectable(open, text)).on_hover_text(path.display().to_string()).clicked() {
                        action = Some(ProjectAction::Open(path.clone()));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add(egui::Button::new(theme::icon(icons::X).size(13.0).color(self.theme.muted)).frame(false)).on_hover_text("Forget this project. The file and its favorites stay.").clicked() {
                            action = Some(ProjectAction::Forget(path.clone()));
                        }
                    });
                });
            }
            ui.separator();
            if ui.selectable_label(false, theme::icon_text(icons::FOLDER_OPEN, "Open…", 14.0, egui::FontFamily::Proportional, Color32::PLACEHOLDER)).clicked() {
                action = Some(ProjectAction::Browse);
            }
            if ui.selectable_label(false, theme::icon_text(icons::PLUS, "New…", 14.0, egui::FontFamily::Proportional, Color32::PLACEHOLDER)).clicked() {
                action = Some(ProjectAction::New);
            }
        });
        action
    }

    pub fn apply_project_action(&mut self, action: ProjectAction) {
        let result = match action {
            ProjectAction::Default => self.config.close_project(),
            ProjectAction::Open(path) => self.config.open_project(&path),
            ProjectAction::Forget(path) => self.config.forget_project(&path),
            ProjectAction::Browse => match rfd::FileDialog::new().add_filter("hark project", &[hark_core::project::EXTENSION]).pick_file() {
                Some(path) => self.config.open_project(&path),
                None => return,
            },
            ProjectAction::New => {
                let dialog = rfd::FileDialog::new().add_filter("hark project", &[hark_core::project::EXTENSION]).set_file_name(format!("project.{}", hark_core::project::EXTENSION));
                let Some(path) = dialog.save_file() else { return };
                self.config.create_project(&path)
            }
        };
        if let Err(e) = result {
            self.set_status(format!("Project: {e:#}"), true);
        }
        self.db.set_project(self.config.project_id());
        self.location = None;
        self.expanded = self.config.roots().iter().map(|r| r.path.clone()).collect();
        self.rescan();
    }

    fn apply_tag_action(&mut self, action: TagAction) {
        match action {
            TagAction::Color(k, hex) => self.config.tags[k].color = Some(hex),
            TagAction::Rename(k, new) => {
                let old = self.config.tags[k].name.clone();
                self.tag_edit = None;
                if !new.is_empty() && new != old && !self.config.tags.iter().any(|t| t.name == new) {
                    match self.db.rename_tag(&old, &new) {
                        Ok(_) => {
                            self.config.tags[k].name = new.clone();
                            for t in self.filter.tags.iter_mut().filter(|t| **t == old) {
                                *t = new.clone();
                            }
                        }
                        Err(e) => self.set_status(format!("Rename tag: {e:#}"), true),
                    }
                }
            }
            TagAction::Move(from, to) => {
                let t = self.config.tags.remove(from);
                self.config.tags.insert(to, t);
            }
            TagAction::Remove(k) => {
                let t = self.config.tags.remove(k);
                if let Err(e) = self.db.remove_tag(&t.name) {
                    self.set_status(format!("Remove tag: {e:#}"), true);
                }
                self.filter.tags.retain(|x| *x != t.name);
            }
            TagAction::Add => {
                let mut name = "new".to_string();
                let mut n = 2;
                while self.config.tags.iter().any(|t| t.name == name) {
                    name = format!("new{n}");
                    n += 1;
                }
                self.config.tags.push(hark_core::TagDef { name: name.clone(), color: None });
                self.tag_edit = Some((self.config.tags.len() - 1, name));
            }
        }
        if let Err(e) = self.config.save() {
            self.set_status(format!("Save config: {e:#}"), true);
        }
        self.list_dirty = true;
    }

    /// A key and what it does, as one dim line reads in a terminal: the key a shade brighter than its word.
    fn key_hint(&self, ui: &mut egui::Ui, key: &str, action: &str) {
        let dim = self.theme.muted.gamma_multiply(0.6);
        ui.label(RichText::new(key).family(egui::FontFamily::Name(theme::MEDIUM.into())).color(self.theme.muted.gamma_multiply(0.85)).size(12.0));
        ui.add_space(-4.0);
        ui.label(RichText::new(action).color(dim).size(12.0));
        ui.add_space(8.0);
    }

    fn ui_status(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if self.status.is_empty() {
                for (key, action) in [("Space", "Play"), ("↑ ↓", "Audition"), ("F", "Favorite"), ("1–9", "Tags"), ("[ ]", "Selection"), ("E", "Export")] {
                    self.key_hint(ui, key, action);
                }
            } else {
                let color = if self.status_is_error { self.theme.err } else { self.theme.muted };
                ui.label(RichText::new(&self.status).color(color).size(12.0));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let device = match (&self.player, &self.audio_error) {
                    (Some(p), _) => theme::icon_text(icons::SPEAKER_HIGH, &format!("{}  ·  {} Hz", p.device_name, p.sample_rate), 12.0, egui::FontFamily::Proportional, self.theme.muted),
                    (None, Some(e)) => theme::icon_text(icons::SPEAKER_HIGH, &format!("No audio: {e}"), 12.0, egui::FontFamily::Proportional, self.theme.err),
                    _ => theme::icon_text(icons::SPEAKER_HIGH, "No audio", 12.0, egui::FontFamily::Proportional, self.theme.err),
                };
                ui.label(device);
                if let Some(n) = self.player.as_ref().map(|p| p.underrun_frames()).filter(|n| *n > 0) {
                    ui.label(RichText::new(format!("{n} underrun frames")).color(self.theme.err).size(12.0)).on_hover_text("Silence played because a streamed file was not decoded in time");
                }
                ui.separator();
                if self.scanning {
                    ui.spinner();
                    ui.label(RichText::new("Scanning").color(self.theme.muted).size(12.0));
                } else {
                    ui.label(RichText::new(format!("{} files", self.file_count)).color(self.theme.muted).size(12.0));
                }
            });
        });
    }
}

fn secs_field(v: &mut f64, max: f64) -> egui::DragValue<'_> {
    egui::DragValue::new(v).speed(0.01).range(0.0..=max).fixed_decimals(3)
}

enum FileAction {
    Favorite,
    Tag(String),
    Reveal,
    CopyPath,
}

/// The per-file menu: favorite, every tag with the carried ones checked, and the file's place
/// on disk. Returns what was clicked; toggles keep the menu open, the rest close it.
fn file_menu(ui: &mut egui::Ui, file: &FileRecord, defs: &[hark_core::TagDef], theme: &Theme) -> Option<FileAction> {
    theme::menu_items(ui);
    let mut action = None;
    let mut fav = file.favorite;
    if theme::menu_check(ui, &mut fav, theme::icon_text(icons::STAR, "Favorite", 14.0, egui::FontFamily::Proportional, Color32::PLACEHOLDER)).on_hover_text("F").changed() {
        action = Some(FileAction::Favorite);
    }
    ui.separator();
    for (k, t) in defs.iter().enumerate() {
        let mut on = file.tags.contains(&t.name);
        let color = theme.color_of(k, Some(t)).gamma_multiply(1.3).to_opaque();
        let label = theme::menu_check(ui, &mut on, RichText::new(&t.name).color(color));
        let label = if k < 9 { label.on_hover_text(format!("key {}", k + 1)) } else { label };
        if label.changed() {
            action = Some(FileAction::Tag(t.name.clone()));
        }
    }
    ui.separator();
    if ui.button(theme::icon_text(icons::FOLDER_OPEN, "Show in folder", 14.0, egui::FontFamily::Proportional, Color32::PLACEHOLDER)).clicked() {
        action = Some(FileAction::Reveal);
        ui.close();
    }
    if ui.button(theme::icon_text(icons::COPY, "Copy path", 14.0, egui::FontFamily::Proportional, Color32::PLACEHOLDER)).clicked() {
        ui.ctx().copy_text(file.path.display().to_string());
        action = Some(FileAction::CopyPath);
        ui.close();
    }
    action
}

/// Opens the file's folder in the desktop's file manager, selecting the file where that is
/// supported.
/// Folders under `root`, a few levels deep, relative and sorted; enough to pick a target from.
fn subfolders(root: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, root: &Path, depth: usize, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        let mut dirs: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.is_dir() && !p.file_name().map(|n| n.to_string_lossy().starts_with('.')).unwrap_or(true)).collect();
        dirs.sort();
        for d in dirs {
            if out.len() >= 300 {
                return;
            }
            if let Ok(rel) = d.strip_prefix(root) {
                out.push(rel.to_path_buf());
            }
            if depth > 1 {
                walk(&d, root, depth - 1, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, 3, &mut out);
    out
}

fn reveal(path: &Path) -> std::io::Result<()> {
    use std::process::Command;
    #[cfg(target_os = "windows")]
    {
        Command::new("explorer").arg(format!("/select,{}", path.display())).spawn()?;
    }
    #[cfg(target_os = "macos")]
    {
        Command::new("open").arg("-R").arg(path).spawn()?;
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let dir = path.parent().unwrap_or(path);
        Command::new("xdg-open").arg(dir).spawn()?;
    }
    Ok(())
}

/// Two lines: what the file becomes, and what is done to the signal on the way.
impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let zoom = ctx.zoom_factor();
        if (zoom - self.config.ui_zoom).abs() > 1e-3 {
            self.config.ui_zoom = zoom;
            let _ = self.config.save();
        }
        self.handle_done();
        self.expire_status(&ctx);
        if self.list_dirty {
            self.refresh_list();
        }
        self.handle_keys(&ctx);

        let pad = theme::PAD;
        let bar = egui::Frame::new().fill(self.theme.bg_deep).inner_margin(Margin::symmetric(pad, 8));
        egui::Panel::top("top").resizable(false).exact_size(TOP_BAR_H + 16.0).frame(bar).show(ui, |ui| {
            self.ui_top(ui);
        });
        egui::Panel::bottom("status").frame(egui::Frame::new().fill(self.theme.bg_deep).inner_margin(Margin::symmetric(pad, 4))).show(ui, |ui| {
            self.ui_status(ui);
        });
        // The export card decides the height of the whole bottom panel.
        let bottom_h = self.export_card_h + 2.0 * theme::CARD_PAD as f32 + 16.0;
        egui::Panel::bottom("bottom").resizable(false).exact_size(bottom_h).frame(egui::Frame::new().fill(self.theme.bg).inner_margin(Margin { left: pad, right: pad, top: 8, bottom: 8 })).show(ui, |ui| {
            self.ui_bottom(ui);
        });
        egui::Panel::left("tree").resizable(true).default_size(250.0).frame(egui::Frame::new().fill(self.theme.bg_deep).inner_margin(Margin { left: 4, right: 4, top: 8, bottom: 8 })).show(ui, |ui| {
            self.ui_tree(ui);
        });
        egui::CentralPanel::default().frame(egui::Frame::new().fill(self.theme.bg).inner_margin(Margin { left: pad, right: pad, top: 8, bottom: 0 })).show(ui, |ui| {
            self.ui_list(ui);
        });

        self.ui_settings(&ctx);

        let analysing = self.current.as_ref().map(|c| c.peaks.is_none()).unwrap_or(false);
        if self.is_playing() || !self.pending.is_empty() || analysing {
            ctx.request_repaint_after(Duration::from_millis(16));
        }
    }
}

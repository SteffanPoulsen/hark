#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod dnd;
mod keys;
mod preset_ui;
mod promise;
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
use hark_core::config::Normalize;
use hark_core::export::{ExportRequest, PreviewLevel, Selection};
use hark_core::pcm::{Channels, DownmixMode, Fades};
use hark_core::library::{CardState, ChannelClass, Query, Sort, SortKey};
use hark_core::copies::CopyRequest;
use hark_core::keys::Action;
use hark_core::{Config, Db, ExportPreset, FileRecord, Loudness, Peaks};

use theme::Theme;
use waveform::{ClipFades, SelectionLevel, WaveEvent, WaveState};
use hark_core::loudness::{Level, Span};
use worker::{Done, Fold, Folded, Job, LoadRequest, Loaded, ScanPhase, Worker};

/// Twenty minutes of stereo at 48 kHz, decoded audio and peaks together.
const CACHE_BYTES: usize = 48_000 * 60 * 20 * 2 * 4;
const PRELOAD: usize = 3;
const EXPORT_CARD_W: f32 = 380.0;
const TREE_W: f32 = 320.0;
/// The bottom panel never grows past leaving the list this much: its heading, header and a
/// handful of rows.
const LIST_MIN_H: f32 = 200.0;
/// The panels whose size the user drags; a layout reset forgets them.
const PANELS: [&str; 3] = ["tree", "bottom", "export"];
/// Where the list's column widths live in egui's memory.
const COLUMNS_ID: &str = "columns";
const STAR_W: f32 = 26.0;
/// Length, channels, rate, bits, codec, LUFS.
const NUMERIC_W: [f32; 6] = [74.0, 30.0, 56.0, 36.0, 80.0, 60.0];

/// The widths the user gave the folder and tag columns; the name column takes what the
/// window leaves, so the list fits any width and scale.
#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
struct ColumnWidths {
    folder: f32,
    tags: f32,
}

impl ColumnWidths {
    const NAME_MIN: f32 = 120.0;
    const MIN: f32 = 60.0;
}

impl Default for ColumnWidths {
    fn default() -> Self {
        ColumnWidths { folder: 140.0, tags: 150.0 }
    }
}

/// A draggable divider, named by the column on its left.
#[derive(Clone, Copy)]
enum Divider {
    Name,
    Folder,
    Tags,
}
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
            "--channels" => region.channels = args.next().and_then(|c| c.parse().ok()),
            "--settings" => {
                settings = Some(match args.next().as_deref() {
                    Some("tags") => settings::Page::Tags,
                    Some("keys") => settings::Page::Keys,
                    Some("presets") | Some("export") => settings::Page::Presets,
                    Some("about") => settings::Page::About,
                    Some("general") | None => settings::Page::General,
                    Some(other) => {
                        open = Some(PathBuf::from(other));
                        settings::Page::General
                    }
                })
            }
            "-h" | "--help" => {
                println!("hark [--open <audio file or folder>] [--in <secs>] [--out <secs>] [--fade-in <secs>] [--fade-out <secs>] [--channels <1,2,…>] [--project <file.hark>] [--settings [general|tags|presets]]");
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
            .with_min_inner_size([900.0, 600.0])
            // hark takes no file drops, and as a drop target the window would accept its own
            // drag and swallow the drop. This reaches Windows; `dnd` unregisters on macOS.
            .with_drag_and_drop(false),
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

type ClipKey = (usize, usize, bool, usize, usize, hark_core::pcm::FadeShape, Option<Channels>, Option<DownmixMode>);

fn clip_key(c: &Clip) -> ClipKey {
    (c.start, c.end, c.looped, c.fade_in, c.fade_out, c.fade_shape, c.channels, c.fold)
}

/// A span's measurement in flight, when it was asked for, and the level it promises.
struct Asked {
    span: Span,
    since: Instant,
    level: promise::Promise<Option<Level>>,
}

/// A copy being written from `recipe` to `output`, and for the clipboard once written.
struct Writing {
    recipe: CopyRequest,
    output: PathBuf,
    clipboard: bool,
}

impl Writing {
    fn span(&self) -> Option<(f64, f64)> {
        self.recipe.selection.map(|s| (s.start_secs, s.end_secs))
    }
}

/// A drag out of the selection waiting for its file.
struct AwaitedDrag {
    /// When the press that began it started, as egui has it.
    press: Option<f64>,
    output: PathBuf,
    image: Option<dnd::Image>,
    /// The pointer meanwhile.
    pending: Option<dnd::Image>,
    what: String,
    written: bool,
    /// A frame showing the file written has been drawn. A Windows drag loop keeps the window
    /// from painting until the drop, so the drag waits for it.
    shown: bool,
}

/// How many span measurements the window keeps; each is a few hundred bytes.
const RECENT_LEVELS: usize = 64;

/// A measurement still running this long after it was asked for shows in the selection; a
/// shorter one lands before anyone would see it.
const MEASURING_SHOWS_AFTER: Duration = Duration::from_millis(150);

/// An envelope a whole-file pass is building: the analysis of `path` (`fold` `None`) or its
/// fold.
struct Building {
    path: PathBuf,
    fold: Option<Fold>,
    peaks: Peaks,
}

/// The takes of a file, found in the channels kept.
struct Takes {
    path: PathBuf,
    channels: Option<Channels>,
    spans: Vec<(f64, f64)>,
}

/// A selection, fades and channels asked for on the command line, applied once the opened
/// file loads.
#[derive(Default, Clone, Copy)]
struct OpenRegion {
    start: Option<f64>,
    end: Option<f64>,
    fade_in: f64,
    fade_out: f64,
    channels: Option<Channels>,
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
    drag: dnd::DragOut,
    /// egui holds a press the desktop has let go of: a drag out of the window took the
    /// release, so winit never reported it, or egui-winit dropped it after the pointer left
    /// the window. The next frame's input gets the release.
    drag_took_release: bool,

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
    /// Where the list was on the last frame; Ctrl+C over it copies the original file.
    list_rect: egui::Rect,

    cache: HashMap<PathBuf, Arc<Loaded>>,
    cache_order: VecDeque<PathBuf>,
    pending: HashSet<PathBuf>,
    current: Option<Arc<Loaded>>,
    wave: WaveState,
    open_region: Option<(PathBuf, OpenRegion)>,
    /// The takes last found, kept for the arrow keys.
    takes: Option<Takes>,
    /// Hear and see the file as the export card would write it: its channels and its level.
    preview: bool,
    /// The card as the database last stored it for this project.
    card_saved: Option<CardState>,
    /// The last file analysis was asked for: coming back to it does not start its pass over.
    analysis_asked: Option<PathBuf>,
    /// The last file folded as the preview hears it, and the last fold asked for, with whether
    /// it was to measure.
    folded: Option<(PathBuf, Fold, Arc<Folded>)>,
    fold_asked: Option<(PathBuf, Fold, bool)>,
    /// The selection's level under the preview: the last span measured, and the measurement
    /// in flight.
    span_level: Option<(Span, Result<Level, String>)>,
    span_asked: Option<Asked>,
    /// Spans measured lately, newest first, so coming back to a file or a selection does not
    /// measure it again.
    recent_levels: VecDeque<Level>,
    /// Copies being written, for the drag or the clipboard.
    writing: Vec<Writing>,
    /// A drag out of the selection whose file is being written. It starts once the file is
    /// there, if the press that began it still holds.
    drag_awaited: Option<AwaitedDrag>,
    /// A selection or fade number field is being dragged, which edits the clip as a drag on
    /// the wave does.
    fields_dragging: bool,
    /// At most one analysis and one fold, each as far as its pass has got. Each kind runs one
    /// pass at a time, so a result of that kind ends whatever of it is still here.
    building: Vec<Building>,
    /// A whole-file pass that failed, shown in the wave until another file is chosen.
    pass_error: Option<(PathBuf, String)>,
    transport_h: f32,
    /// Height the export card's content took on the last frame; the bottom panel is sized to it.
    export_card_h: f32,
    /// What the export button reports: the write under way, or how the last one went.
    export_state: ExportState,
    /// Panel sizes and column widths go back to their defaults on the next frame.
    reset_layout: bool,
    /// The scale slider's value while it is being dragged; applied when the drag ends.
    zoom_pending: Option<f32>,
    gain_db: f32,
    /// The preset the card's values came from; `None` once they were edited.
    preset_name: Option<String>,
    /// The values the next export uses.
    preset: ExportPreset,
    overwrite: bool,
    status: String,
    status_since: Instant,
    status_is_error: bool,
    /// The running scan's phase; `None` between scans.
    scanning: Option<ScanPhase>,
    settings_open: bool,
    settings_page: settings::Page,
    /// The output devices the General page offers, listed once per opening of the window.
    outputs: Option<Vec<hark_core::audition::Output>>,
    update_check: Option<settings::UpdateCheck>,
    /// The clear-copies confirmation is up.
    copies_clear: bool,
    /// The mark beside the wordmark, white on transparent, tinted at draw time.
    mark: egui::TextureHandle,
    /// Centre line of the project switcher on the last frame; the brand sits on it, since
    /// egui does not centre buttons on the row they are in.
    search_id: egui::Id,
    tag_edit: Option<(usize, String)>,
    /// The file menu's new-tag field.
    new_tag: String,
    /// The action whose key the settings window is waiting for.
    key_capture: Option<Action>,
    /// The bounds the voice was given when it was last played from the selection, which it
    /// follows while it plays; `None` after a play outside the selection.
    synced: Option<ClipKey>,
    /// The clip played from outside the selection, which keeps its own bounds.
    outside: Option<Clip>,
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
        let (player, audio_error) = match Player::open(config.output_sample_rate, config.output_device.as_deref()) {
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
        let scanning = (!config.roots().is_empty()).then_some(ScanPhase::Walking { seen: 0 });
        let expanded = config.roots().iter().map(|r| r.path.clone()).collect();
        let mut app = App {
            config,
            theme,
            db,
            player,
            audio_error,
            worker,
            drag: dnd::DragOut::new(cc),
            drag_took_release: false,
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
            list_rect: egui::Rect::NOTHING,
            cache: HashMap::new(),
            cache_order: VecDeque::new(),
            pending: HashSet::new(),
            current: None,
            wave: WaveState::default(),
            open_region: None,
            synced: None,
            outside: None,
            takes: None,
            preview: true,
            card_saved: None,
            analysis_asked: None,
            folded: None,
            fold_asked: None,
            span_level: None,
            span_asked: None,
            recent_levels: VecDeque::new(),
            writing: Vec::new(),
            drag_awaited: None,
            fields_dragging: false,
            building: Vec::new(),
            pass_error: None,
            transport_h: 34.0,
            export_card_h: 380.0,
            export_state: ExportState::Idle,
            reset_layout: false,
            zoom_pending: None,
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
            outputs: None,
            update_check: None,
            mark,
            settings_page: settings::Page::General,
            copies_clear: false,
            search_id: egui::Id::new("search"),
            tag_edit: None,
            new_tag: String::new(),
            key_capture: None,
            play_on_load: false,
            export_name: String::new(),
            preset_edit: preset_name,
            preset_rename: None,
        };
        app.restore_card();
        app
    }

    fn card_state(&self) -> CardState {
        CardState { preset_name: self.preset_name.clone(), preset: self.preset.clone(), preview: self.preview, overwrite: self.overwrite }
    }

    /// Sets the card and Preview as the open project left them; a project never opened starts
    /// on the default preset.
    fn restore_card(&mut self) {
        let state = self.db.card_state().unwrap_or_else(|e| {
            self.set_status(format!("Export card: {e:#}"), true);
            None
        });
        match state {
            Some(s) => {
                match s.preset_name.and_then(|n| self.config.preset(&n).cloned().map(|p| (n, p))) {
                    Some((name, preset)) => (self.preset, self.preset_name) = (preset, Some(name)),
                    None => (self.preset, self.preset_name) = (s.preset, None),
                }
                self.preview = s.preview;
                self.overwrite = s.overwrite;
            }
            None => {
                let (name, preset) = self.config.default_preset();
                (self.preset, self.preset_name) = (preset.clone(), Some(name));
            }
        }
        self.card_saved = Some(self.card_state());
    }

    fn remember_card(&mut self) {
        let state = self.card_state();
        if self.card_saved.as_ref() != Some(&state) {
            if let Err(e) = self.db.set_card_state(&state) {
                self.set_status(format!("Export card: {e:#}"), true);
            }
            self.card_saved = Some(state);
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
    /// Selects the next or previous take in the file, the takes being its spans of sound
    /// parted by silence, and plays it when autoplay is on: Up and Down browse the list, Left
    /// and Right browse inside a file.
    fn step_take(&mut self, dir: i32) {
        let Some(cur) = &self.current else { return };
        let Source::Whole(pcm) = &cur.source else {
            self.set_status("Takes need the file decoded whole; this one streams", true);
            return;
        };
        let kept = self.wave.channels;
        if self.takes.as_ref().is_none_or(|t| t.path != cur.path || t.channels != kept) {
            self.takes = Some(Takes { path: cur.path.clone(), channels: kept, spans: pcm.takes(kept) });
        }
        let duration = cur.duration_secs;
        let takes = &self.takes.as_ref().expect("just found").spans;
        let eps = 1e-3;
        let next = match (self.wave.selection, dir > 0) {
            (None, true) => takes.first().copied(),
            (None, false) => takes.last().copied(),
            (Some((a, _)), true) => takes.iter().find(|t| t.0 > a + eps).copied(),
            (Some((a, _)), false) => takes.iter().rev().find(|t| t.0 < a - eps).copied(),
        };
        if takes.is_empty() {
            self.set_status("Silent; no takes", true);
            return;
        }
        let Some(take) = next else { return };
        self.wave.selection = Some(take);
        self.wave.reveal(take, duration);
        if self.config.autoplay {
            self.play(None);
        }
    }

    /// Shrinks the selection, or the whole file, to the sound in it.
    fn trim_selection(&mut self) {
        let Some(cur) = &self.current else { return };
        let Source::Whole(pcm) = &cur.source else {
            self.set_status("Trim needs the file decoded whole; this one streams", true);
            return;
        };
        let (start, end) = self.wave.selection.unwrap_or((0.0, cur.duration_secs));
        match pcm.sound_span(start, end, self.wave.channels) {
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

    fn root_paths(&self) -> Vec<PathBuf> {
        self.config.roots().iter().map(|r| r.path.clone()).collect()
    }

    /// The files the Favorites view lists, so the badge never counts a favorite outside the folders.
    fn count_favorites(&mut self) {
        let q = Query { favorites_only: true, scope: Some(self.root_paths()), ..Query::default() };
        self.fav_count = self.db.count(&q).unwrap_or(0);
    }

    fn refresh_list(&mut self) {
        self.list_dirty = false;
        self.count_favorites();
        let mut q = Query {
            favorites_only: self.filter.favorites,
            tags: self.filter.tags.clone(),
            extensions: self.filter.extensions(),
            channels: self.filter.channels.clone(),
            sort: self.config.list_sort,
            ..Query::default()
        };
        if !self.search.trim().is_empty() {
            q.text = self.search.clone();
            q.scope = Some(self.root_paths());
            q.limit = SEARCH_LIMIT;
        } else {
            match &self.location {
                Some(Location::Folder(d)) => q.under = Some(d.clone()),
                Some(Location::Favorites) => {
                    q.favorites_only = true;
                    q.scope = Some(self.root_paths());
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

    /// `selected` for the file the user is waiting on, which loads on a thread of its own even
    /// when a preload of it is already queued.
    fn request_load(&mut self, path: &Path, selected: bool) {
        if self.cache.contains_key(path) || (!selected && self.pending.contains(path)) {
            return;
        }
        self.pending.insert(path.to_path_buf());
        let preview = (self.preview && (self.preset.mono.is_some() || !matches!(self.preset.normalize, Normalize::None))).then(|| self.preset.clone());
        let req = LoadRequest { path: path.to_path_buf(), device_rate: self.device_rate(), stream_above_mb: self.config.stream_above_mb, preview };
        if !selected {
            self.worker.send(Job::Preload(req));
        } else if let Some(skipped) = self.worker.load_selected(req).filter(|p| p != path) {
            self.pending.remove(&skipped);
        }
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
            self.stop();
            self.synced = None;
            self.export_name.clear();
            if let Some(l) = self.cache.get(&path).cloned() {
                self.show_loaded(l);
                if play {
                    self.play(None);
                }
            } else {
                self.request_load(&path, true);
                self.current = None;
                self.wave.reset(self.files[i].info.as_ref().map(|x| x.duration_secs).unwrap_or(1.0));
            }
        } else if play {
            self.play(None);
        }
        for d in 1..=PRELOAD {
            if let Some(f) = self.files.get(i + d) {
                self.request_load(&f.path.clone(), false);
            }
            if i >= d {
                if let Some(f) = self.files.get(i - d) {
                    self.request_load(&f.path.clone(), false);
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
            self.wave.channels = r.channels.and_then(|c| Channels::pick(c.iter(), l.channels));
        }
        let recorded = self.record_of(&l.path).and_then(FileRecord::loudness).is_some();
        if l.loudness.is_some() && !recorded {
            self.record_loudness(&l.path, l.loudness.as_ref().map(|l| l.loudness));
        }
        let measured = l.loudness.is_some() || recorded;
        if (l.peaks.is_none() || !measured) && self.analysis_asked.as_ref() != Some(&l.path) {
            self.worker.analyze(l.path.clone(), l.peaks.clone());
            self.analysis_asked = Some(l.path.clone());
        }
        if let Some((fold, f)) = &l.folded {
            self.folded = Some((l.path.clone(), *fold, f.clone()));
            self.fold_asked = Some((l.path.clone(), *fold, f.loudness.is_some()));
        }
        if self.pass_error.as_ref().is_some_and(|(p, _)| *p != l.path) {
            self.pass_error = None;
        }
        self.current = Some(l);
    }

    fn record_of(&self, path: &Path) -> Option<&FileRecord> {
        self.files.iter().find(|f| f.path == path)
    }

    /// The preview still lacks the fold of the current file, or the measurement its level
    /// comes from.
    fn preview_pending(&self) -> bool {
        if self.current.is_none() || !self.preview {
            return false;
        }
        (self.preview_fold().is_some() && self.folded_exact().is_none()) || (self.previewing_level() && self.preview_measurement().is_none())
    }

    /// What the card folds the current file to: its kept channels, by the preset's downmix.
    fn preview_fold(&self) -> Option<Fold> {
        self.preset.mono.map(|mode| Fold { mode, channels: self.wave.channels })
    }

    /// The clip is being edited by a drag, on the wave or on a number field: what follows an
    /// edit waits for it to end.
    fn editing(&self) -> bool {
        self.wave.dragging() || self.fields_dragging
    }

    /// The current file's kept channels folded by the preset's own downmix.
    fn folded_exact(&self) -> Option<&Arc<Folded>> {
        let (cur, fold) = (self.current.as_ref()?, self.preview_fold()?);
        self.folded.as_ref().filter(|(p, f, _)| *p == cur.path && *f == fold).map(|(_, _, f)| f)
    }

    /// Loudness of the current file's kept channels as the preset folds them: its fold's,
    /// measured with the fold and under another downmix while the one asked for is built; with
    /// some channels muted and no fold, the whole file's measured with them muted; or the
    /// file's own. Never a measurement of other channels.
    fn preview_measurement(&self) -> Option<Loudness> {
        let cur = self.current.as_ref()?;
        let kept = self.wave.channels;
        match (self.preset.mono, kept) {
            (Some(_), _) => self.folded.as_ref().filter(|(p, f, _)| *p == cur.path && f.channels == kept).and_then(|(_, _, f)| f.loudness.as_ref()).map(|l| l.loudness),
            (None, Some(_)) => {
                let whole = |s: &Span| s.path == cur.path && s.secs.is_none() && s.channels == kept && s.fold.is_none() && s.fades.is_zero();
                self.recent_levels.iter().find(|l| whole(&l.span) && l.stamp == cur.stamp).map(|l| l.loudness)
            }
            (None, None) => cur.loudness.as_ref().map(|l| l.loudness).or_else(|| self.record_of(&cur.path).and_then(FileRecord::loudness)),
        }
    }

    fn preview_level(&self) -> Option<PreviewLevel> {
        self.preview_measurement().filter(|_| self.previewing_level()).map(|l| hark_core::export::preview_level_from(&l, &self.preset))
    }

    /// What the preview's level is of when the passes over the whole file cannot give it: the
    /// selection, the file's kept channels, the whole file under its fades, or the whole file
    /// at a rate the preset resamples it to; shaped as an export shapes it.
    fn preview_span(&self) -> Option<Span> {
        let cur = self.current.as_ref()?;
        let w = &self.wave;
        let fades = Fades { in_secs: w.fade_in, out_secs: w.fade_out, shape: w.fade_shape };
        let span = Span::new(&cur.path, w.selection, fades, self.preset.mono, self.preset.sample_rate, cur.source_rate).picking(w.channels);
        (span.secs.is_some() || span.channels.is_some() || !fades.is_zero() || span.rate.is_some()).then_some(span)
    }

    /// Where a span starts and ends in seconds.
    fn span_bounds(&self, span: &Span) -> (f64, f64) {
        span.secs.unwrap_or((0.0, self.current.as_ref().map(|c| c.duration_secs).unwrap_or(0.0)))
    }

    /// The level the preview's span has: the preset's level of it once measured, and until
    /// then that of the measured span it is an edit of, so a drag and the measurement after
    /// it change nothing until the result lands. `None` is the file's own level.
    fn selection_level(&self) -> Option<PreviewLevel> {
        let span = self.preview_span().filter(|_| self.previewing_level())?;
        let (measured, result) = self.span_level.as_ref()?;
        let ((a, b), (c, d)) = (self.span_bounds(measured), self.span_bounds(&span));
        let edit_of = measured.path == span.path && measured.channels == span.channels && measured.fold == span.fold && measured.rate == span.rate && a < d && c < b;
        (*measured == span || edit_of).then(|| result.as_ref().ok()).flatten().map(|l| hark_core::export::preview_level_from(&l.loudness, &self.preset))
    }

    /// Every measurement held of the current file, for an export or a copy to take the one
    /// of exactly its audio instead of measuring it again.
    fn levels_held(&self) -> Vec<Level> {
        let Some(cur) = self.current.as_ref() else { return Vec::new() };
        let folded = self.folded.as_ref().and_then(|(_, _, f)| f.loudness.as_ref());
        let file = cur.loudness.as_ref();
        self.recent_levels.iter().chain(folded).chain(file).filter(|l| l.span.path == cur.path).cloned().collect()
    }

    /// A span measured lately, of the file as it is loaded now.
    fn recent_level(&self, span: &Span) -> Option<&Level> {
        let cur = self.current.as_ref()?;
        self.recent_levels.iter().find(|l| l.span == *span && l.stamp == cur.stamp)
    }

    /// How the wave draws the selection: at its own level once measured, as far as the
    /// measurement has got until then, and as the rest of the file when nothing is selected or
    /// the preview's level is off.
    fn wave_selection_level(&self) -> SelectionLevel {
        let gain = hark_core::pcm::db_to_gain(self.clip_level_db());
        if self.wave.selection.is_some() && self.exporting(self.wave.selection) {
            return SelectionLevel::Busy { gain, what: "Exporting" };
        }
        let Some(span) = self.preview_span().filter(|_| self.previewing_level() && self.wave.selection.is_some()) else { return SelectionLevel::Same };
        // The selection draws at the level it plays at. Nothing shows through a drag or a
        // measurement quick enough to land unseen; only one still running after
        // `MEASURING_SHOWS_AFTER` shows itself.
        match self.span_asked.as_ref().filter(|a| a.span == span && a.since.elapsed() >= MEASURING_SHOWS_AFTER) {
            Some(_) => SelectionLevel::Busy { gain, what: "Measuring level" },
            None => SelectionLevel::Gain(gain),
        }
    }

    /// Flashes what was just written on the wave, when it is of the file on it.
    fn flash_written(&mut self, source: &Path, span: Option<(f64, f64)>) {
        if let Some(cur) = self.current.as_ref().filter(|c| c.path == source) {
            self.wave.flash(span.unwrap_or((0.0, cur.duration_secs)));
        }
    }

    /// Whether a copy or an export of `span` of the current file, or of all of it, is being
    /// written.
    fn exporting(&self, span: Option<(f64, f64)>) -> bool {
        let Some(cur) = self.current.as_ref() else { return false };
        let export = matches!(&self.export_state, ExportState::Writing { source, span: s } if *source == cur.path && *s == span);
        export || self.writing.iter().any(|w| w.recipe.source == cur.path && w.span() == span)
    }

    /// The selection with its fades as a clip, or the whole file without one.
    fn selection_clip(&self) -> Option<Clip> {
        let (Some(player), Some(cur)) = (&self.player, &self.current) else { return None };
        let rate = player.sample_rate as f64;
        let total = cur.source.frames(player.sample_rate);
        let frames = |t: f64| ((t * rate) as usize).min(total);
        let (start, end) = match self.wave.selection {
            Some((a, b)) => (frames(a), frames(b)),
            None => (0, total),
        };
        let fold = if self.preview { self.preset.mono } else { None };
        let w = &self.wave;
        Some(Clip { source: cur.source.clone(), start, end: end.max(start), looped: self.config.looping, fade_in: frames(w.fade_in), fade_out: frames(w.fade_out), fade_shape: w.fade_shape, channels: w.channels, fold })
    }

    /// Asks the worker for what the preview of the current file needs and does not have yet.
    fn request_preview(&mut self) {
        let Some(cur) = self.current.as_ref().filter(|_| self.preview) else { return };
        if let Some(fold) = self.preview_fold() {
            let measure = !matches!(self.preset.normalize, Normalize::None);
            let have = self.folded_exact().map(|f| f.loudness.is_some() || !measure).unwrap_or(false);
            let asked = |m: bool| self.fold_asked.as_ref() == Some(&(cur.path.clone(), fold, m));
            if !have && !asked(true) && !asked(measure) {
                self.worker.fold(cur, fold, measure);
                self.fold_asked = Some((cur.path.clone(), fold, measure));
            }
        }
        // The span's level waits for the selection and fades to rest: a drag would replace
        // the request every frame.
        let Some(span) = self.preview_span().filter(|_| self.previewing_level() && !self.editing()) else { return };
        let measured = self.span_level.as_ref().is_some_and(|(s, _)| *s == span);
        let asked = self.span_asked.as_ref().is_some_and(|a| a.span == span);
        if measured || asked {
            return;
        }
        if let Some(level) = self.recent_level(&span).cloned() {
            self.span_level = Some((span, Ok(level)));
            return;
        }
        // Samples in hand are the export's own only at the file's rate; otherwise the span is
        // read from the file as an export reads it. A fold in hand is of the kept channels;
        // the file's own samples are of them all.
        let held = match (&cur.source, span.fold) {
            (Source::Whole(pcm), _) if pcm.sample_rate != cur.source_rate || span.rate.is_some() => None,
            (Source::File { .. }, _) => None,
            (Source::Whole(_), None) if span.channels.is_some() => None,
            (Source::Whole(pcm), None) => Some(pcm.clone()),
            (Source::Whole(_), Some(_)) => match self.folded_exact() {
                Some(f) => f.samples.clone(),
                None => return,
            },
        };
        let level = self.worker.measure_span(span.clone(), held.map(|pcm| (pcm, cur.stamp)));
        self.span_asked = Some(Asked { span, since: Instant::now(), level });
    }

    /// The preview's level owns playback and the waveform: the preset normalises.
    fn previewing_level(&self) -> bool {
        self.preview && !matches!(self.preset.normalize, Normalize::None)
    }

    /// The fades the wave draws the clip under: none with the preview off or nothing to fade,
    /// the handles' own when the preview has no level to wait for, and the measured span's
    /// otherwise.
    fn wave_clip_fades(&self) -> Option<ClipFades> {
        let span = self.preview_span().filter(|s| self.preview && (s.secs.is_some() || !s.fades.is_zero()))?;
        if !self.previewing_level() {
            return Some(ClipFades { secs: self.span_bounds(&span), fades: span.fades });
        }
        let (measured, _) = self.span_level.as_ref().filter(|(s, _)| s.path == span.path)?;
        Some(ClipFades { secs: self.span_bounds(measured), fades: measured.fades })
    }

    /// The gain the file plays and draws at, in dB: the preset's level of the whole file under
    /// the preview (unity until its first level comes, and for silence), the fader otherwise.
    fn file_level_db(&self) -> f64 {
        if !self.previewing_level() {
            return self.gain_db as f64;
        }
        self.preview_level().and_then(|l| l.gain_db).unwrap_or(0.0)
    }

    /// The gain the selection plays and draws at, in dB: the preset's level of the selection
    /// once measured, the file's until then and without a selection.
    fn clip_level_db(&self) -> f64 {
        match self.selection_level() {
            Some(l) => l.gain_db.unwrap_or(0.0),
            None => self.file_level_db(),
        }
    }

    /// The gain playback runs at, in dB: the selection's, or the file's while playing outside
    /// the selection.
    fn level_db(&self) -> f64 {
        if self.is_playing() && self.synced.is_none() {
            self.file_level_db()
        } else {
            self.clip_level_db()
        }
    }

    fn play(&mut self, from_secs: Option<f64>) {
        let (Some(player), Some(clip)) = (&self.player, self.selection_clip()) else { return };
        let total = clip.source.frames(player.sample_rate);
        let from = from_secs.map(|t| ((t * player.sample_rate as f64) as usize).min(total)).unwrap_or(clip.start);
        // The fades belong to the selection, or to the whole file without one. A seek outside
        // the selection plays the file from there without them.
        let inside = from >= clip.start && from < clip.end;
        let clip = if inside { clip } else { Clip { start: from, end: total, fade_in: 0, fade_out: 0, ..clip } };
        self.synced = inside.then(|| clip_key(&clip));
        self.outside = (!inside).then(|| clip.clone());
        player.play_from(clip, from);
    }

    /// A playing selection follows its edits: new bounds, fades and loop state reach the
    /// voice as they are made. Playback outside the selection keeps its bounds and follows the
    /// channels kept and the fold. A streamed voice starts again where it is instead, once the
    /// drag that moved it ends.
    fn sync_clip(&mut self) {
        if !self.is_playing() {
            return;
        }
        let clip = match self.synced {
            Some(key) => match self.selection_clip() {
                Some(clip) if clip_key(&clip) != key => clip,
                _ => return,
            },
            None => {
                let Some(mut clip) = self.outside.clone() else { return };
                let shape = (self.wave.channels, self.preset.mono.filter(|_| self.preview));
                if (clip.channels, clip.fold) == shape {
                    return;
                }
                (clip.channels, clip.fold) = shape;
                clip
            }
        };
        let Some(player) = &self.player else { return };
        if clip.source.is_streamed() {
            if self.editing() {
                return;
            }
            player.play_from(clip.clone(), player.position());
        } else {
            player.set_bounds(&clip);
        }
        match self.synced {
            Some(_) => self.synced = Some(clip_key(&clip)),
            None => self.outside = Some(clip),
        }
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
                self.count_favorites();
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


    fn zoom_to_selection(&mut self) {
        if let Some(cur) = &self.current {
            self.wave.zoom_to_selection(cur.duration_secs);
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
        req.channels = self.wave.channels;
        req.fade_in_secs = self.wave.fade_in;
        req.fade_out_secs = self.wave.fade_out;
        req.gain_db = self.gain_db as f64;
        req.preset.fade_shape = self.wave.fade_shape;
        req.overwrite = self.overwrite;
        req.levels = self.levels_held();
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
            let channels = if preset.mono.is_some() { 1 } else { req.channels.map_or(info.channels, |c| c.count() as u16) };
            let rate = preset.sample_rate.unwrap_or(info.sample_rate);
            let bytes = secs * rate as f64 * channels as f64 * preset.bits as f64 / 8.0;
            out.push_str(&if bytes >= 1e6 { format!("  ·  ≈ {:.1} MB", bytes / 1e6) } else { format!("  ·  ≈ {:.0} KB", bytes / 1e3) });
        }
        out
    }

    fn export(&mut self) {
        if matches!(self.export_state, ExportState::Writing { .. }) {
            return;
        }
        match self.export_request() {
            Some(Ok(req)) => {
                let span = req.selection.map(|s| (s.start_secs, s.end_secs));
                self.export_state = ExportState::Writing { source: req.source.clone(), span };
                self.set_status(format!("Exporting {}", req.output_path().display()), false);
                self.worker.send(Job::Export(Box::new(req)));
            }
            Some(Err(e)) => self.set_status(format!("Export: {e}"), true),
            None => {}
        }
    }

    /// The selection as heard, bound for the copy folder.
    fn copy_request(&self) -> Option<CopyRequest> {
        let f = &self.files[self.selected?];
        let mut req = CopyRequest::from_config(&self.config, &f.path, self.preset.clone(), f.tags.clone(), f.favorite);
        req.source_hash = f.hash.clone();
        req.selection = self.wave.selection.map(|(a, b)| Selection { start_secs: a, end_secs: b });
        req.channels = self.wave.channels;
        req.fade_in_secs = self.wave.fade_in;
        req.fade_out_secs = self.wave.fade_out;
        req.preset.fade_shape = self.wave.fade_shape;
        req.gain_db = self.gain_db as f64;
        req.levels = self.levels_held();
        Some(req)
    }

    /// The measurement of the span a copy would take, while it is still running.
    fn measuring(&self) -> Option<promise::Promise<Option<Level>>> {
        let span = self.preview_span()?;
        self.span_asked.as_ref().filter(|a| a.span == span).map(|a| a.level.clone())
    }

    /// Puts the selection on the clipboard as a file: at once when a copy from this recipe is
    /// there, else once the worker has written it.
    fn copy_selection(&mut self) {
        let Some(req) = self.copy_request() else { return };
        if let Some(w) = self.writing.iter_mut().find(|w| w.recipe.same_recipe(&req)) {
            w.clipboard = true;
            let what = tilde(&w.output);
            return self.set_status(format!("Copying {what}"), false);
        }
        match req.plan() {
            Ok(plan) if plan.pending.is_none() => self.copy_out(vec![plan.output]),
            Ok(plan) => {
                self.set_status(format!("Copying {}", tilde(&plan.output)), false);
                let _ = self.write_copy(&req, plan, true);
            }
            Err(e) => self.set_status(format!("Copy: {e:#}"), true),
        }
    }

    /// Queues a planned copy and shows it under way on the wave until it is written. `false`
    /// when the copy could not be queued, which the status line then says.
    fn write_copy(&mut self, req: &CopyRequest, plan: hark_core::copies::Plan, clipboard: bool) -> bool {
        let output = plan.output.clone();
        if let Err(plan) = self.worker.copy(plan, self.measuring()) {
            (*plan).abandon();
            self.set_status("Copy failed: the copy thread has stopped".to_string(), true);
            return false;
        }
        self.writing.push(Writing { recipe: req.clone(), output, clipboard });
        true
    }

    /// Drags the selection out: at once when a copy from this recipe is there, else once the
    /// worker has written it, if the button is still held then.
    fn drag_selection(&mut self, ctx: &egui::Context, image: Option<dnd::Image>, pending: Option<dnd::Image>) {
        let Some(req) = self.copy_request() else { return };
        let output = match self.writing.iter().find(|w| w.recipe.same_recipe(&req)) {
            Some(w) => w.output.clone(),
            None => {
                let plan = match req.plan() {
                    Ok(p) => p,
                    Err(e) => return self.set_status(format!("Copy: {e:#}"), true),
                };
                let output = plan.output.clone();
                if plan.pending.is_none() {
                    return self.drag_out(vec![output.clone()], image, &tilde(&output));
                }
                if !self.write_copy(&req, plan, false) {
                    return;
                }
                output
            }
        };
        let what = tilde(&output);
        self.drag_awaited = Some(AwaitedDrag { press: ctx.input(|i| i.pointer.press_start_time()), output, image, pending, what, written: false, shown: false });
    }

    /// Starts the awaited drag once its file is written, or lets it go when its press ends
    /// first.
    fn hold_drag(&mut self, ctx: &egui::Context) {
        let Some(drag) = &mut self.drag_awaited else { return self.drag.done_waiting(ctx) };
        let (down, released, press) = ctx.input(|i| (i.pointer.primary_down(), i.pointer.primary_released(), i.pointer.press_start_time()));
        let held = self.drag.button_held();
        if !held || !down || released || press != drag.press {
            self.drag_took_release |= down && !released && !held;
            let drag = self.drag_awaited.take().expect("checked above");
            self.drag.done_waiting(ctx);
            let when = if drag.written { "the drag started" } else { "it was ready" };
            self.set_status(format!("Released {} before {when}; drag again to take it", drag.what), false);
        } else if drag.shown {
            let drag = self.drag_awaited.take().expect("checked above");
            self.drag.done_waiting(ctx);
            self.drag_out(vec![drag.output], drag.image, &drag.what);
        } else {
            if drag.written && !drag.shown {
                drag.shown = true;
                ctx.request_repaint();
            }
            self.drag.waiting(ctx, drag.pending.as_ref());
        }
    }

    /// Hands `paths` to the drop target of the drag the pointer is in the middle of.
    fn drag_out(&mut self, paths: Vec<PathBuf>, image: Option<dnd::Image>, what: &str) {
        match self.drag.start(paths, image) {
            Ok(()) => {
                self.drag_took_release = true;
                self.set_status(format!("Dragging {what}"), false);
            }
            Err(e) => self.set_status(format!("Drag: {e}; the file lands at {what}"), true),
        }
    }

    /// The picture under the pointer while a span of a file is dragged out: its envelope on a
    /// translucent card, at the display's scale.
    fn ghost(&self, ctx: &egui::Context, peaks: &hark_core::Peaks, span: (f64, f64), kept: Option<Channels>, gain_at: &dyn Fn(f64) -> f32) -> dnd::Image {
        let scale = ctx.pixels_per_point().round().max(1.0) as i32;
        let colors = waveform::GhostColors { card: self.theme.bg_deep, peak: self.theme.accent.gamma_multiply(0.7), rms: self.theme.accent, hatch: None };
        waveform::ghost(peaks, span, kept, gain_at, &colors, scale)
    }

    /// The pointer while a drag waits for its file: the drag's picture greyed on a striped
    /// card, hanging from the pointer as the drag's own does.
    fn pending_ghost(&self, ctx: &egui::Context, peaks: &hark_core::Peaks, span: (f64, f64), kept: Option<Channels>, gain_at: &dyn Fn(f64) -> f32) -> dnd::Image {
        // AppKit takes a cursor's pixels as points.
        let scale = if cfg!(target_os = "macos") { 1 } else { ctx.pixels_per_point().round().max(1.0) as i32 };
        let colors = waveform::GhostColors { card: self.theme.bg_deep, peak: self.theme.muted.gamma_multiply(0.5), rms: self.theme.muted, hatch: Some(self.theme.line) };
        waveform::ghost(peaks, span, kept, gain_at, &colors, scale)
    }

    fn copy_out(&mut self, paths: Vec<PathBuf>) {
        let shown = paths.iter().map(|p| tilde(p)).collect::<Vec<_>>().join(", ");
        match dnd::copy_files(&paths) {
            Ok(()) => self.set_status(format!("Copied {shown}"), false),
            Err(e) => self.set_status(format!("Copy: {e}"), true),
        }
    }

    /// Ctrl+C: over the list, the original file; elsewhere, the selection as a copy.
    fn copy_shortcut(&mut self, ctx: &egui::Context) {
        let over_list = ctx.pointer_latest_pos().is_some_and(|p| self.list_rect.contains(p));
        let Some(i) = self.selected else { return };
        if over_list {
            let path = self.files[i].path.clone();
            self.copy_out(vec![path]);
        } else {
            self.copy_selection();
        }
    }

    fn record_loudness(&mut self, path: &Path, loudness: Option<Loudness>) {
        let Some(ld) = loudness else { return };
        let _ = self.db.set_loudness(path, &ld);
        if let Some(i) = self.files.iter().position(|f| f.path == path) {
            self.files[i].lufs = Some(ld.integrated_lufs);
            self.files[i].true_peak_dbfs = Some(ld.true_peak_dbfs);
            self.files[i].sample_peak_dbfs = Some(ld.sample_peak_dbfs);
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
                            let wanted = self.selected.and_then(|i| self.files.get(i)).is_some_and(|f| f.path == path);
                            self.request_load(&path, wanted);
                        }
                        Ok(l) => {
                            self.remember(l.clone());
                            let wanted = self.selected.and_then(|i| self.files.get(i)).map(|f| f.path == path).unwrap_or(false);
                            if wanted && self.current.as_ref().map(|c| c.path != path).unwrap_or(true) {
                                self.show_loaded(l);
                                if self.config.autoplay || std::mem::take(&mut self.play_on_load) {
                                    self.play(None);
                                }
                            }
                        }
                        Err(e) => self.set_status(format!("{}: {e}", path.display()), true),
                    }
                }
                Done::Analyzed(path, result) => {
                    self.building.retain(|b| b.fold.is_some());
                    match result {
                        Ok((peaks, loudness)) => {
                            self.record_loudness(&path, loudness.as_ref().map(|l| l.loudness));
                            let current = self.current.clone().filter(|c| c.path == path);
                            if let Some(old) = self.cache.get(&path).cloned().or(current) {
                                let l = Arc::new(Loaded { path: old.path.clone(), stamp: old.stamp, source_rate: old.source_rate, channels: old.channels, duration_secs: old.duration_secs, source: old.source.clone(), peaks: Some(peaks), loudness, folded: old.folded.clone() });
                                if self.cache.contains_key(&path) {
                                    self.cache.insert(path.clone(), l.clone());
                                }
                                if self.current.as_ref().is_some_and(|c| c.path == path) {
                                    self.current = Some(l);
                                }
                            }
                        }
                        Err(e) => {
                            self.set_status(format!("{}: {e}", path.display()), true);
                            self.pass_error = Some((path, e));
                        }
                    }
                }
                Done::Folded { path, fold, measure, result } => {
                    self.building.retain(|b| b.fold.is_none());
                    if self.fold_asked.as_ref() == Some(&(path.clone(), fold, measure)) {
                        match result {
                            Ok(f) => self.folded = Some((path, fold, Arc::new(f))),
                            Err(e) => {
                                self.set_status(format!("Preview of {}: {e}", path.display()), true);
                                self.pass_error = Some((path, e));
                            }
                        }
                    }
                }
                Done::Partial { path, fold, from, sample_rate, bins } => {
                    if from == 0 {
                        self.building.retain(|b| b.fold.is_some() != fold.is_some());
                        let channels = bins.len() as u16;
                        self.building.push(Building { path, fold, peaks: Peaks { sample_rate, channels, bin_frames: Peaks::DEFAULT_BIN, total_frames: 0, bins } });
                    } else if let Some(b) = self.building.iter_mut().find(|b| b.path == path && b.fold == fold && b.peaks.bins.first().map(|c| c.len()) == Some(from)) {
                        for (have, new) in b.peaks.bins.iter_mut().zip(bins) {
                            have.extend(new);
                        }
                    }
                }
                Done::SpanMeasured(span, result) => {
                    if let Ok(level) = &result {
                        self.recent_levels.retain(|l| l.span != level.span);
                        self.recent_levels.push_front(level.clone());
                        self.recent_levels.truncate(RECENT_LEVELS);
                    }
                    if self.span_asked.as_ref().is_some_and(|a| a.span == span) {
                        self.span_asked = None;
                        if let Err(e) = &result {
                            self.set_status(format!("Level of the selection: {e}"), true);
                        }
                        self.span_level = Some((span, result));
                    }
                }
                Done::Copied { output, result } => {
                    let written = self.writing.iter().position(|w| w.output == output).map(|i| self.writing.remove(i));
                    let awaited = self.drag_awaited.as_mut().filter(|d| d.output == output);
                    match result {
                        Ok(r) => {
                            if let Some(d) = awaited {
                                d.written = true;
                            }
                            if let Some(w) = written {
                                self.flash_written(&w.recipe.source, w.span());
                                if w.clipboard {
                                    self.copy_out(vec![r.output]);
                                }
                            }
                        }
                        Err(e) => {
                            if awaited.is_some() {
                                self.drag_awaited = None;
                            }
                            self.set_status(format!("Copy failed: {e}"), true);
                        }
                    }
                }
                Done::Exported(result) => {
                    let written = match std::mem::replace(&mut self.export_state, ExportState::Idle) {
                        ExportState::Writing { source, span } => Some((source, span)),
                        _ => None,
                    };
                    match result {
                        Ok(r) => {
                            let mut gain = format!("{:+.2} dB", r.origin.measured.applied_gain_db);
                            for w in &r.warnings {
                                gain.push_str("  ·  ");
                                gain.push_str(w);
                            }
                            let shown = self.config.output_root().and_then(|root| r.output.strip_prefix(root).ok()).unwrap_or(&r.output);
                            self.set_status(format!("Exported {}  ·  {gain}", shown.display()), false);
                            self.export_state = ExportState::Done { at: Instant::now() };
                            if let Some((source, span)) = written {
                                self.flash_written(&source, span);
                            }
                        }
                        Err(e) => {
                            self.set_status(format!("Export failed: {e}"), true);
                            self.export_state = ExportState::Failed { at: Instant::now() };
                        }
                    }
                }
                Done::Scanning(phase) => {
                    if self.scanning.is_some() {
                        self.scanning = Some(phase);
                    }
                }
                Done::Scanned(reports) => {
                    self.scanning = None;
                    let scanned = reports.iter().filter_map(|(_, r)| r.as_ref().ok());
                    self.file_count = scanned.clone().map(|r| r.seen).sum();
                    let errors: usize = scanned.map(|r| r.errors.len()).sum();
                    let failed: Vec<String> = reports.iter().filter_map(|(name, r)| r.as_ref().err().map(|e| format!("{name}: {e}"))).collect();
                    let status = if !failed.is_empty() {
                        failed.join("  ·  ")
                    } else if errors > 0 {
                        format!("{errors} files could not be read")
                    } else {
                        String::new()
                    };
                    self.set_status(status, errors > 0 || !failed.is_empty());
                    self.list_dirty = true;
                }
            }
        }
    }

    fn handle_keys(&mut self, ctx: &egui::Context) {
        if ctx.egui_wants_keyboard_input() || self.key_capture.is_some() {
            return;
        }
        let n = self.files.len();
        let pressed = |k: Key| ctx.input(|i| i.key_pressed(k));
        let shift = ctx.input(|i| i.modifiers.shift);
        let map = self.config.keys.clone();
        let fired = |a: Action| keys::pressed(ctx, &map, a);
        // egui-winit turns Ctrl+C into its own event in place of the key press.
        if ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Copy))) {
            self.copy_shortcut(ctx);
            return;
        }
        let go = |app: &mut App, i: usize| app.select(i, app.config.autoplay);
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
        if fired(Action::NextTake) {
            self.step_take(1);
        }
        if fired(Action::PreviousTake) {
            self.step_take(-1);
        }
        if fired(Action::Play) {
            if self.is_playing() && !shift {
                self.stop();
            } else {
                self.play(None);
            }
        }
        if fired(Action::PlayFromStart) {
            self.play(Some(0.0));
        }
        if fired(Action::Favorite) {
            self.toggle_favorite();
        }
        if fired(Action::Loop) {
            self.config.looping = !self.config.looping;
            let _ = self.config.save();
        }
        if fired(Action::Autoplay) {
            self.config.autoplay = !self.config.autoplay;
            let _ = self.config.save();
        }
        if fired(Action::Preview) {
            self.preview = !self.preview;
        }
        if fired(Action::Export) {
            self.export();
        }
        if fired(Action::Zoom) {
            self.zoom_to_selection();
        }
        if fired(Action::Trim) {
            self.trim_selection();
        }
        // Escape closes an open menu first; egui sees to that, and the selection stays.
        if fired(Action::ClearSelection) && !egui::Popup::is_any_open(ctx) {
            self.wave.selection = None;
            self.wave.fade_in = 0.0;
            self.wave.fade_out = 0.0;
        }
        if let Some(cur) = &self.current {
            let pos = self.player.as_ref().map(|p| p.position() as f64 / p.sample_rate as f64);
            if let Some(t) = pos {
                if fired(Action::SelectionStart) {
                    let end = self.wave.selection.map(|(_, b)| b).filter(|b| *b > t).unwrap_or(cur.duration_secs);
                    self.wave.selection = Some((t, end));
                }
                if fired(Action::SelectionEnd) {
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

    /// The action's chord as a hint reads it: `Ctrl+C`, `←`, `Space`.
    fn key_of(&self, action: Action) -> String {
        keys::hint(self.config.keys.chord(action))
    }

    // ---- widgets ----


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
                .stroke(Stroke::new(1.0, if focused { self.theme.accent2 } else { self.theme.line }))
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
            self.scanning = None;
            self.list_dirty = true;
            return;
        }
        self.scanning = Some(ScanPhase::Walking { seen: 0 });
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
        let tags = self.config.tags.clone();
        // The tags scroll before they can run off the window; the other columns are short.
        let max_h = (ui.ctx().content_rect().height() - button.rect.bottom() - 160.0).max(120.0);
        const TAGS_W: f32 = 300.0;
        const COLUMN_W: f32 = 110.0;
        popup.show(|ui| {
            theme::menu_items(ui);
            changed |= theme::menu_check(ui, &mut self.filter.favorites, theme::icon_text(icons::STAR, "Favorites", 14.0, egui::FontFamily::Proportional, Color32::PLACEHOLDER)).changed();
            ui.separator();
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(TAGS_W);
                    theme::menu_heading(ui, &theme, "Tags");
                    ui.add_space(4.0);
                    if tags.is_empty() {
                        ui.horizontal_wrapped(|ui| {
                            ui.add_space(theme::MENU_INSET);
                            ui.label(RichText::new("No tags yet. A file's menu makes one.").color(theme.muted).size(12.5));
                        });
                    }
                    egui::ScrollArea::vertical().max_height(max_h).auto_shrink([false, true]).show(ui, |ui| {
                        egui::Frame::new().inner_margin(Margin { left: theme::MENU_INSET as i8, ..Margin::ZERO }).show(ui, |ui| ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
                            for (k, t) in tags.iter().enumerate() {
                                let on = self.filter.tags.contains(&t.name);
                                if chip(ui, &theme, &t.name, theme.color_of(k, Some(t)), on, true).clicked() {
                                    toggle_in(&mut self.filter.tags, t.name.clone());
                                    changed = true;
                                }
                            }
                        }));
                    });
                });
                ui.vertical(|ui| {
                    ui.set_width(COLUMN_W);
                    theme::menu_heading(ui, &theme, "File type");
                    for (name, _) in FILE_TYPES {
                        let mut on = self.filter.types.contains(&name);
                        if theme::menu_check(ui, &mut on, name).changed() {
                            toggle_in(&mut self.filter.types, name);
                            changed = true;
                        }
                    }
                });
                ui.vertical(|ui| {
                    ui.set_width(COLUMN_W);
                    theme::menu_heading(ui, &theme, "Channels");
                    for c in ChannelClass::ALL {
                        let mut on = self.filter.channels.contains(&c);
                        if theme::menu_check(ui, &mut on, c.name()).changed() {
                            toggle_in(&mut self.filter.channels, c);
                            changed = true;
                        }
                    }
                });
            });
            ui.separator();
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
        // With the panel's own 4 at the sides and 8 at the bottom, the buttons get 12 all round.
        let bar = egui::Frame::new().inner_margin(Margin { left: 8, right: 8, top: 12, bottom: 4 });
        egui::Panel::bottom("library-actions").resizable(false).frame(bar).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let w = (ui.available_width() - 6.0) / 2.0;
                if ui.add(theme::tool_button_widget(icons::FOLDER_PLUS, "Add folder").min_size(egui::vec2(w, 30.0))).on_hover_text("Add a library folder").clicked() {
                    self.add_root();
                }
                let can = self.scanning.is_none() && !self.config.roots().is_empty();
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
                let area = egui::Rect::from_min_max(ui.cursor().min, ui.clip_rect().max);
                let title = if self.config.active.is_some() { "No folders in this project" } else { "No library folders yet" };
                ui.scope_builder(egui::UiBuilder::new().max_rect(area.shrink2(egui::vec2(24.0, 0.0))).layout(egui::Layout::top_down(egui::Align::Center)), |ui| {
                    // A little above the middle, where a centred block looks centred.
                    ui.add_space(((area.height() - 110.0) * 0.42).max(16.0));
                    ui.label(theme::icon(icons::FOLDER_DASHED).size(44.0).color(self.theme.muted));
                    ui.add_space(10.0);
                    ui.label(theme::semibold(title).size(15.0));
                    ui.add_space(4.0);
                    ui.label(RichText::new("Add a folder of sounds below.").color(self.theme.muted).size(12.5));
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
        let mut dragged: Option<usize> = None;
        let mut fav_toggle: Option<usize> = None;
        self.list_rect = ui.max_rect();
        let mut action: Option<(usize, FileAction)> = None;
        let defs = self.config.tags.clone();
        let favorite_key = self.key_of(Action::Favorite);
        let mut new_tag = std::mem::take(&mut self.new_tag);
        let shown_folder = match &self.location {
            Some(Location::Folder(d)) if self.search.trim().is_empty() => Some(d.clone()),
            _ => None,
        };
        let nested = shown_folder.as_deref().map(|base| self.files.iter().any(|f| f.path.parent() != Some(base))).unwrap_or(true);
        // The folder and tag columns resize by the dividers in the header, which this code
        // owns; the name column takes what the window leaves, and the numeric columns fit
        // their widest value.
        let header_h = 24.0;
        // The gap between columns; a divider sits in its middle, so half of it pads the text.
        ui.spacing_mut().item_spacing.x = 12.0;
        let spacing = ui.spacing().item_spacing.x;
        let widths_id = egui::Id::new(COLUMNS_ID);
        let mut widths: ColumnWidths = ui.data_mut(|d| d.get_persisted(widths_id)).unwrap_or_default();
        let fixed_w: f32 = STAR_W + NUMERIC_W.iter().sum::<f32>() + spacing * (10 + nested as usize) as f32;
        // The name takes what is left after the fixed columns, the folder and the tags. When
        // that falls under its floor, the tags give way, then the folder, each down to its
        // floor; only past that does the right edge clip.
        let mut shown = ColumnWidths { folder: if nested { widths.folder } else { 0.0 }, tags: widths.tags };
        let mut name_w = ui.available_width() - fixed_w - shown.folder - shown.tags;
        if name_w < ColumnWidths::NAME_MIN {
            let mut over = ColumnWidths::NAME_MIN - name_w;
            let give = (shown.tags - ColumnWidths::MIN).clamp(0.0, over);
            shown.tags -= give;
            over -= give;
            if nested {
                shown.folder -= (shown.folder - ColumnWidths::MIN).clamp(0.0, over);
            }
            name_w = ColumnWidths::NAME_MIN;
        }
        // The dividers' lines span the header row's rect grown the way the table grows a row's
        // stripe, half the spacing each way, so they line up with the rows.
        let header_rect = egui::Rect::from_min_size(ui.cursor().min, egui::vec2(ui.available_width(), header_h));
        let band = egui::emath::GuiRounding::round_ui(header_rect.expand2(0.5 * ui.spacing().item_spacing));
        let table_rect = egui::Rect::from_min_max(band.min, egui::pos2(band.right(), ui.max_rect().bottom()));
        let mut builder = TableBuilder::new(ui)
            .striped(true)
            .sense(Sense::click_and_drag())
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::exact(STAR_W))
            .column(Column::exact(name_w).clip(true))
            .columns(Column::exact(shown.folder).clip(true), nested as usize)
            .column(Column::exact(shown.tags).clip(true));
        for w in NUMERIC_W {
            builder = builder.column(Column::exact(w));
        }
        builder = builder.column(Column::remainder());
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
        let config = &self.config;
        let folder_of = |f: &FileRecord| -> Option<PathBuf> {
            let dir = f.path.parent()?;
            match &shown_folder {
                Some(base) => dir.strip_prefix(base).ok().map(Path::to_path_buf),
                None => config.root_of(&f.path).and_then(|r| dir.strip_prefix(&r.path).ok().map(|rel| Path::new(&r.name).join(rel))),
            }
        };
        let chip = |ui: &mut egui::Ui, text: &str, color: Color32| {
            let font = FontId::new(11.0, egui::FontFamily::Name(theme::MEDIUM.into()));
            let galley = ui.painter().layout_no_wrap(text.to_string(), font, Color32::WHITE);
            let pad = egui::vec2(7.0, 2.0);
            let (rect, _) = ui.allocate_exact_size(galley.size() + pad * 2.0, Sense::hover());
            ui.painter().rect_filled(rect, CornerRadius::same(9), color.gamma_multiply(0.28));
            ui.painter().galley(rect.min + pad, galley, color.gamma_multiply(1.5).to_opaque());
        };
        let sort = self.config.list_sort;
        let mut sort_by: Option<SortKey> = None;
        let headers: [(&str, Option<SortKey>); 11] = [
            ("", None),
            ("Name", Some(SortKey::Name)),
            ("Folder", Some(SortKey::Folder)),
            ("Tags", None),
            ("Length", Some(SortKey::Length)),
            ("Ch", Some(SortKey::Channels)),
            ("Rate", Some(SortKey::Rate)),
            ("Bits", Some(SortKey::Bits)),
            ("Codec", Some(SortKey::Codec)),
            ("LUFS", Some(SortKey::Loudness)),
            ("", None),
        ];
        builder
            .header(header_h, |mut h| {
                for (title, key) in headers.into_iter().filter(|(t, _)| nested || *t != "Folder") {
                    h.col(|ui| {
                        let Some(key) = key else {
                            ui.label(RichText::new(title).color(theme.muted).size(12.0));
                            return;
                        };
                        let r = ui.interact(ui.max_rect(), ui.id().with("sort"), Sense::click()).on_hover_text(format!("Sort by {}", key.name()));
                        if r.hovered() {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                        }
                        let color = if r.hovered() || sort.key == key { theme.text } else { theme.muted };
                        let text: egui::WidgetText = if sort.key == key {
                            theme::text_icon(title, if sort.descending { icons::CARET_DOWN } else { icons::CARET_UP }, 12.0, color).into()
                        } else {
                            RichText::new(title).color(color).size(12.0).into()
                        };
                        ui.add(egui::Label::new(text).selectable(false));
                        if r.clicked() {
                            sort_by = Some(key);
                        }
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
                        ui.with_layout(egui::Layout::centered_and_justified(egui::Direction::LeftToRight), |ui| {
                            ui.label(icon.color(color).size(15.0));
                        });
                    });
                    if star.clicked() {
                        fav_toggle = Some(i);
                    }
                    row.col(|ui| {
                        let text = if f.error.is_some() { RichText::new(&f.name).color(theme.err) } else { RichText::new(&f.name) };
                        ui.label(text);
                    });
                    if nested {
                        row.col(|ui| {
                            if let Some(folder) = folder_of(f) {
                                folder_label(ui, &folder, theme.muted);
                            }
                        });
                    }
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
                    row.col(|_| {});
                    let resp = row.response();
                    if fav_toggle == Some(i) {
                    } else if resp.drag_started() {
                        dragged = Some(i);
                    } else if resp.double_clicked() {
                        double = Some(i);
                    } else if resp.clicked() {
                        clicked = Some(i);
                    } else if resp.secondary_clicked() {
                        context = Some(i);
                    }
                    egui::Popup::context_menu(&resp).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
                        if let Some(a) = file_menu(ui, f, &defs, &theme, &favorite_key, &mut new_tag) {
                            action = Some((i, a));
                        }
                    });
                });
            });
        // The dividers, after the table so they sit above its rows: a tick in the header at
        // every column edge, and a grab on the three that resize, which shows its line down
        // the whole table while hovered or dragged. A drag trades width between the columns
        // on either side of the divider, so nothing else moves; as the name column holds the
        // window's slack, the one after the tags trades with it.
        let mut column_ws: Vec<(f32, Option<Divider>)> = vec![(STAR_W, None), (name_w, Some(Divider::Name))];
        if nested {
            column_ws.push((shown.folder, Some(Divider::Folder)));
        }
        column_ws.push((shown.tags, Some(Divider::Tags)));
        column_ws.extend(NUMERIC_W.map(|w| (w, None)));
        let painter = ui.painter().with_clip_rect(table_rect);
        let mut x = header_rect.left();
        let inner = column_ws.len() - 1;
        for (n, (w, resize)) in column_ws.into_iter().enumerate() {
            x += w;
            let divider = painter.round_to_pixel_center(x + spacing * 0.5);
            x += spacing;
            let Some(col) = resize else {
                if n > 0 && n < inner {
                    painter.vline(divider, header_rect.y_range().shrink(7.0), Stroke::new(1.0, theme.line));
                }
                continue;
            };
            let grab = egui::Rect::from_center_size(egui::pos2(divider, header_rect.center().y), egui::vec2(10.0, header_h));
            let r = ui.interact(grab, widths_id.with(n), Sense::click_and_drag());
            let active = r.hovered() || r.dragged();
            if active {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeColumn);
                painter.vline(divider, table_rect.y_range(), Stroke::new(1.0, theme.text));
            } else {
                painter.vline(divider, header_rect.y_range().shrink(3.0), Stroke::new(1.0, theme.muted));
            }
            let floor = ColumnWidths::MIN;
            let defaults = ColumnWidths::default();
            if r.double_clicked() {
                match col {
                    Divider::Name if nested => widths.folder = defaults.folder,
                    Divider::Name | Divider::Tags => widths.tags = defaults.tags,
                    Divider::Folder => widths = defaults,
                }
                ui.data_mut(|d| d.insert_persisted(widths_id, widths));
            } else if r.dragged() {
                let dx = r.drag_delta().x;
                match col {
                    Divider::Name if nested => widths.folder = shown.folder - dx.clamp(ColumnWidths::NAME_MIN - name_w, shown.folder - floor),
                    Divider::Name => widths.tags = shown.tags - dx.clamp(ColumnWidths::NAME_MIN - name_w, shown.tags - floor),
                    Divider::Folder => {
                        let dx = dx.clamp(floor - shown.folder, shown.tags - floor);
                        widths.folder = shown.folder + dx;
                        widths.tags = shown.tags - dx;
                    }
                    Divider::Tags => widths.tags = shown.tags + dx.clamp(floor - shown.tags, name_w - ColumnWidths::NAME_MIN),
                }
                ui.data_mut(|d| d.insert_persisted(widths_id, widths));
            }
        }
        self.new_tag = new_tag;
        if let Some(i) = fav_toggle {
            self.toggle_favorite_of(i);
        }
        if let Some(i) = dragged {
            let path = self.files[i].path.clone();
            let what = self.files[i].name.clone();
            let image = self.cache.get(&path).and_then(|l| l.peaks.clone()).map(|p| self.ghost(ui.ctx(), &p, (0.0, p.duration_secs()), None, &|_| 1.0));
            self.drag_out(vec![path], image, &what);
        } else if let Some(i) = double {
            self.select(i, true);
        } else if let Some(i) = clicked {
            self.select(i, self.config.autoplay);
        } else if let Some(i) = context {
            if self.selected != Some(i) {
                self.select(i, false);
            }
        }
        if let Some((i, a)) = action {
            self.apply_file_action(i, a);
        }
        if let Some(key) = sort_by {
            let descending = sort.key == key && !sort.descending;
            self.config.list_sort = Sort { key, descending };
            let _ = self.config.save();
            self.list_dirty = true;
            self.scroll_to_selected = true;
        }
    }

    fn apply_file_action(&mut self, i: usize, action: FileAction) {
        let Some(f) = self.files.get(i) else { return };
        match action {
            FileAction::Favorite => self.toggle_favorite_of(i),
            FileAction::Tag(t) => self.toggle_tag_of(i, &t),
            FileAction::NewTag(name) => {
                // A name already defined is put on the file as it is.
                if self.config.tag(&name).is_none() {
                    self.config.tags.push(hark_core::TagDef { name: name.clone(), color: None });
                    if let Err(e) = self.config.save() {
                        self.set_status(format!("Save config: {e:#}"), true);
                    }
                }
                if !self.files[i].tags.contains(&name) {
                    self.toggle_tag_of(i, &name);
                }
            }
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
        // The gap between the cards is split between the two panels, so the resize handle on
        // the export panel's edge sits in the middle of it.
        let export_max = (ui.available_width() - 420.0).max(EXPORT_CARD_W);
        egui::Panel::right("export").resizable(true).default_size(EXPORT_CARD_W).size_range(320.0..=export_max).show_separator_line(false).frame(egui::Frame::new().inner_margin(Margin { left: theme::PAD / 2, right: 0, top: 0, bottom: 0 })).show(ui, |ui| {
            card.show(ui, |ui| {
                ui.set_min_height(ui.available_height());
                self.ui_export_card(ui);
            });
        });
        egui::CentralPanel::default().frame(egui::Frame::new().inner_margin(Margin { left: 0, right: theme::PAD / 2, top: 0, bottom: 0 })).show(ui, |ui| {
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
                let loudness = c.loudness.as_ref().map(|l| (l.loudness.integrated_lufs, l.loudness.true_peak_dbfs, l.loudness.momentary)).or_else(|| record.and_then(|f| Some((f.lufs?, f.true_peak_dbfs?, false))));
                if let Some((lufs, tp, momentary)) = loudness {
                    meta.push_str(&format!("  ·  {lufs:.1} LUFS{}  ·  {tp:.1} dBTP", if momentary { " (M)" } else { "" }));
                }
                if c.source.is_streamed() {
                    meta.push_str("  ·  streaming");
                }
                ui.add(egui::Label::new(RichText::new(meta).color(self.theme.muted)).truncate());
                if self.selected.is_some() {
                    if ui.available_width() >= chips_w {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| self.tag_chips(ui, true));
                    } else {
                        chips_below = true;
                    }
                }
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
        // Whatever the wave still waits on is written across it, over the envelope as far as
        // its pass has got, dimmed.
        let mut status: Option<(String, bool)> = None;
        let mut partial = None;
        let (peaks, samples) = match &self.current {
            None => {
                let loading = self.selected.and_then(|i| self.files.get(i)).is_some_and(|f| self.pending.contains(&f.path));
                status = Some(if loading { ("Loading".into(), true) } else { ("No file".into(), false) });
                (None, None)
            }
            Some(c) => {
                let fold = self.preview_fold().filter(|_| self.preview);
                let done = match fold {
                    Some(_) => self.folded_exact().map(|f| (f.peaks.clone(), f.samples.clone())),
                    None => c.peaks.clone().map(|p| {
                        let samples = match &c.source {
                            Source::Whole(p) => Some(p.clone()),
                            Source::File { .. } => None,
                        };
                        (p, samples)
                    }),
                };
                match done {
                    Some((p, s)) => (Some(p), s),
                    None => {
                        status = Some((if fold.is_some() { "Building preview" } else { "Analysing" }.into(), true));
                        partial = self.building.iter().position(|b| b.path == c.path && b.fold == fold);
                        let other_fold = self.folded.as_ref().filter(|(p, _, _)| fold.is_some() && *p == c.path).map(|(_, _, f)| f.peaks.clone());
                        (other_fold.filter(|_| partial.is_none()), None)
                    }
                }
            }
        };
        if status.is_none() && self.wave.selection.is_none() && self.exporting(None) {
            status = Some(("Exporting".into(), true));
        }
        if status.is_none() && self.preview_pending() {
            status = Some(("Measuring level".into(), true));
        }
        if let (Some(c), Some((p, e))) = (&self.current, &self.pass_error) {
            if *p == c.path && status.as_ref().is_some_and(|(_, busy)| *busy) {
                status = Some((e.clone(), false));
            }
        }
        let colors = waveform::WaveColors {
            peak: self.theme.accent.gamma_multiply(0.55),
            rms: self.theme.accent.gamma_multiply(0.95),
            selection: self.theme.accent2,
            fade: self.theme.accent2.gamma_multiply(1.3).to_opaque(),
            playhead: self.theme.err,
            axis: self.theme.line,
            flash: self.theme.ok,
        };
        // The gain fader stands beside the waveform and scales it, so what you hear is what
        // you see.
        const FADER_W: f32 = 36.0;
        let level_db = self.level_db();
        let inert = self.previewing_level();
        // Without a selection the whole file is the clip, fades and all; with one, the file
        // around it keeps its own level.
        self.wave.gain = hark_core::pcm::db_to_gain(if self.wave.selection.is_some() { self.file_level_db() } else { self.clip_level_db() });
        self.wave.selection_level = self.wave_selection_level();
        // Taken up between drags only, so the picture moves when a drag ends, as the level does.
        if !self.editing() {
            self.wave.clip_fades = self.wave_clip_fades();
        }
        if let Some(left) = self.span_asked.as_ref().map(|a| MEASURING_SHOWS_AFTER.saturating_sub(a.since.elapsed())).filter(|left| !left.is_zero()) {
            ui.ctx().request_repaint_after(left);
        }
        let play_gain = hark_core::pcm::db_to_gain(level_db);
        if let Some(p) = self.player.as_ref().filter(|p| p.gain() != play_gain) {
            p.set_gain(play_gain);
        }
        let with_warnings = |head: &str, l: PreviewLevel| std::iter::once(head.to_string()).chain(l.warnings).collect::<Vec<_>>().join("\n");
        let of = match (self.wave.selection.is_some(), self.wave.fade_in > 0.0 || self.wave.fade_out > 0.0) {
            (true, _) => "the selection",
            (false, true) => "the file under its fades",
            (false, false) => "the file",
        };
        let settled = self.preview_span().is_some_and(|s| self.span_level.as_ref().is_some_and(|(m, _)| *m == s));
        let level_hint = match (self.preview_span().is_some(), settled, self.selection_level(), self.preview_level()) {
            (true, true, Some(l), _) => with_warnings(&format!("Previewing the preset's level of {of}"), l),
            (true, _, _, _) => format!("Previewing the preset's level: measuring {of}"),
            (false, _, _, Some(l)) => with_warnings("Previewing the preset's level", l),
            (false, _, _, None) => "Previewing the preset's level: measuring the file".to_string(),
        };
        let mut event = WaveEvent::None;
        let mut wave_resp = None;
        let source_channels = self.current.as_ref().map_or(1, |c| c.channels);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            if source_channels > 1 {
                self.channel_strip(ui, source_channels, wave_h);
            }
            let wave_w = ui.available_width() - FADER_W - 8.0;
            ui.allocate_ui_with_layout(egui::vec2(wave_w, wave_h), egui::Layout::top_down(egui::Align::Min), |ui| {
                let peaks = partial.map(|i| &self.building[i].peaks).or(peaks.as_deref());
                let status = status.as_ref().map(|(text, busy)| (text.as_str(), *busy));
                let (ev, resp) = waveform::show(ui, &mut self.wave, peaks, samples.as_deref(), duration, playhead, wave_h, status, &colors);
                event = ev;
                wave_resp = Some(resp);
            });
            // The readout sits level with the ruler and the rail with the wave itself.
            ui.allocate_ui_with_layout(egui::vec2(FADER_W, wave_h), egui::Layout::top_down(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                let readout = ui.allocate_response(egui::vec2(FADER_W, waveform::LANE_H), egui::Sense::hover()).on_hover_text("dB");
                // Under the preview's level the preset sets the gain, so the fader only shows it.
                let (rail, fader) = ui.allocate_exact_size(egui::vec2(FADER_W, wave_h - waveform::LANE_H), if inert { Sense::hover() } else { Sense::click_and_drag() });
                let fader = fader.on_hover_text(if inert { level_hint.as_str() } else { "Volume; right-click or double-click resets" });
                const MIN_DB: f32 = -36.0;
                const MAX_DB: f32 = 12.0;
                let (top, bottom) = (rail.top() + 10.0, rail.bottom() - 10.0);
                let y_of = |db: f32| bottom - (db.clamp(MIN_DB, MAX_DB) - MIN_DB) / (MAX_DB - MIN_DB) * (bottom - top);
                if !inert {
                    if fader.double_clicked() || fader.secondary_clicked() {
                        self.gain_db = 0.0;
                    } else if let Some(p) = fader.interact_pointer_pos().filter(|_| fader.dragged() || fader.clicked()) {
                        self.gain_db = ((MIN_DB + (bottom - p.y) / (bottom - top) * (MAX_DB - MIN_DB)).clamp(MIN_DB, MAX_DB) * 2.0).round() / 2.0;
                    }
                }
                let hot = !inert && (fader.hovered() || fader.dragged());
                if hot {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
                }
                let x = rail.center().x;
                let p = ui.painter();
                p.vline(x, top..=bottom, Stroke::new(2.0, self.theme.line));
                p.hline((x - 6.0)..=(x + 6.0), y_of(0.0), Stroke::new(1.0, self.theme.muted));
                let ink = if inert { self.theme.accent } else if hot { self.theme.text } else { self.theme.muted };
                p.rect_filled(egui::Rect::from_center_size(egui::pos2(x, y_of(level_db as f32)), egui::vec2(14.0, 6.0)), 2.0, ink);
                p.text(egui::pos2(x, readout.rect.center().y), egui::Align2::CENTER_CENTER, format!("{level_db:+.0}"), FontId::proportional(11.0), if inert { self.theme.accent } else { self.theme.muted });
            });
        });
        // The selection's menu, on a right-click anywhere on the wave.
        if let Some(resp) = wave_resp.filter(|_| self.current.is_some()) {
            let mut action: Option<WaveAction> = None;
            egui::Popup::context_menu(&resp).close_behavior(egui::PopupCloseBehavior::CloseOnClick).show(|ui| {
                theme::menu_items(ui);
                let has_selection = self.wave.selection.is_some();
                let item = |ui: &mut egui::Ui, icon: &str, label: &str, key: &str, enabled: bool| ui.add_enabled(enabled, egui::Button::new(theme::icon_text(icon, label, 14.0, egui::FontFamily::Proportional, Color32::PLACEHOLDER))).on_hover_text(key).clicked();
                if item(ui, icons::COPY, "Copy", &format!("{}+C; {}-drag the wave to drag it out", keys::COMMAND, keys::DRAG_COPY), true) {
                    action = Some(WaveAction::Copy);
                }
                ui.separator();
                if item(ui, icons::ARROWS_IN_LINE_HORIZONTAL, "Trim to the sound", &self.key_of(Action::Trim), true) {
                    action = Some(WaveAction::Trim);
                }
                if item(ui, icons::MAGNIFYING_GLASS_PLUS, "Zoom to selection", &self.key_of(Action::Zoom), has_selection) {
                    action = Some(WaveAction::Zoom);
                }
                if item(ui, icons::X, "Clear selection", &self.key_of(Action::ClearSelection), has_selection) {
                    action = Some(WaveAction::Clear);
                }
            });
            match action {
                Some(WaveAction::Copy) => self.copy_selection(),
                Some(WaveAction::Trim) => self.trim_selection(),
                Some(WaveAction::Zoom) => self.zoom_to_selection(),
                Some(WaveAction::Clear) => {
                    self.wave.selection = None;
                    self.wave.fade_in = 0.0;
                    self.wave.fade_out = 0.0;
                }
                None => {}
            }
        }
        match event {
            WaveEvent::Seek(t) => self.play(Some(t)),
            WaveEvent::DragOut => {
                let gain = hark_core::pcm::db_to_gain(self.clip_level_db());
                let fades = self.wave.clip_fades;
                let gain_at = |t: f64| gain * fades.map_or(1.0, |f| f.gain(t));
                let bounds = self.wave.bounds(duration);
                // A folded envelope is already of the kept channels alone.
                let kept = |p: &Peaks| self.wave.channels.filter(|_| p.channels > 1);
                let image = peaks.as_ref().map(|p| self.ghost(ui.ctx(), p, bounds, kept(p), &gain_at));
                let pending = peaks.as_ref().map(|p| self.pending_ghost(ui.ctx(), p, bounds, kept(p), &gain_at));
                self.drag_selection(ui.ctx(), image, pending);
            }
            WaveEvent::None => {}
        }
        let row = ui.scope(|ui| self.ui_transport(ui, playing, playhead, duration)).response.rect.height();
        if (row - self.transport_h).abs() > 0.5 {
            self.transport_h = row;
            ui.ctx().request_repaint();
        }
    }

    /// A toggle per channel of the file beside the wave, each level with its channel's lane:
    /// a click mutes or unmutes the channel, an Alt-click hears it alone, or all of them again
    /// when it already was. What is kept is heard, drawn and exported; the last channel kept
    /// stays.
    fn channel_strip(&mut self, ui: &mut egui::Ui, channels: u16, height: f32) {
        const STRIP_W: f32 = 22.0;
        let (rect, _) = ui.allocate_exact_size(egui::vec2(STRIP_W, height), Sense::hover());
        let n = (channels as usize).min(Channels::MAX);
        let lanes = egui::Rect::from_min_max(egui::pos2(rect.left(), rect.top() + waveform::LANE_H), rect.max);
        let lane_h = lanes.height() / n as f32;
        let button_h = (lane_h - 4.0).clamp(4.0, 22.0);
        let kept = |c: usize| self.wave.channels.is_none_or(|k| k.contains(c));
        let mut next: Option<Vec<usize>> = None;
        for c in 0..n {
            let name = match (channels, c) {
                (2, 0) => "L".to_string(),
                (2, _) => "R".to_string(),
                _ => (c + 1).to_string(),
            };
            let center = egui::pos2(lanes.center().x, lanes.top() + lane_h * (c as f32 + 0.5));
            let button = egui::Rect::from_center_size(center, egui::vec2(STRIP_W, button_h));
            let what = match (channels, c) {
                (2, 0) => "The left channel".to_string(),
                (2, _) => "The right channel".to_string(),
                _ => format!("Channel {}", c + 1),
            };
            let resp = ui.interact(button, ui.id().with(("channel", c)), Sense::click()).on_hover_text(format!("{what}: click mutes or unmutes it; Alt-click hears it alone"));
            let on = kept(c);
            let fill = match (on, resp.hovered()) {
                (true, true) => self.theme.accent2.gamma_multiply(0.45),
                (true, false) => self.theme.accent2.gamma_multiply(0.3),
                (false, true) => self.theme.select,
                (false, false) => Color32::TRANSPARENT,
            };
            let stroke = Stroke::new(1.0, if on { self.theme.accent2.gamma_multiply(0.9) } else { self.theme.line });
            let p = ui.painter();
            p.rect(button, CornerRadius::same(4), fill, stroke, StrokeKind::Inside);
            if button_h >= 12.0 {
                p.text(center, egui::Align2::CENTER_CENTER, name, FontId::new(11.0, egui::FontFamily::Name(theme::MEDIUM.into())), if on { self.theme.text } else { self.theme.muted });
            }
            if resp.clicked() {
                let now: Vec<usize> = (0..n).filter(|&k| kept(k)).collect();
                next = Some(if ui.input(|i| i.modifiers.alt) {
                    if now == [c] { (0..n).collect() } else { vec![c] }
                } else if on {
                    now.into_iter().filter(|&k| k != c).collect()
                } else {
                    now.into_iter().chain([c]).collect()
                });
            }
        }
        if let Some(next) = next.filter(|k| !k.is_empty()) {
            self.wave.channels = Channels::pick(next, channels);
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
        let favorite_key = self.key_of(Action::Favorite);
        let file = self.files[i].clone();
        let mut action = None;
        let mut new_tag = std::mem::take(&mut self.new_tag);
        let theme = self.theme.clone();
        let mut menu_button = |ui: &mut egui::Ui, action: &mut Option<FileAction>| {
            let b = theme::tool_button(ui, icons::TAG, "Tags").on_hover_text("Favorite, tags and more; also on right-click of a row");
            egui::Popup::menu(&b).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
                *action = file_menu(ui, &file, &defs, &theme, &favorite_key, &mut new_tag);
            });
        };
        let chips: Vec<(usize, &str)> = file.tags.iter().map(|t| (defs.iter().position(|d| &d.name == t).unwrap_or(defs.len()), t.as_str())).collect();
        if reversed {
            menu_button(ui, &mut action);
            for (k, t) in chips.iter().rev() {
                chip(ui, &theme, t, theme.color_of(*k, defs.get(*k)), true, false);
            }
        } else {
            for (k, t) in &chips {
                chip(ui, &theme, t, theme.color_of(*k, defs.get(*k)), true, false);
            }
            menu_button(ui, &mut action);
        }
        self.new_tag = new_tag;
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
            if icon_btn(ui, icons::REPEAT, self.config.looping).on_hover_text(format!("Loop ({})", self.key_of(Action::Loop))).clicked() {
                self.config.looping = !self.config.looping;
                let _ = self.config.save();
            }
            if ui.add(egui::Button::new("Auto").selected(self.config.autoplay).min_size(egui::vec2(0.0, CONTROL_H))).on_hover_text(format!("Autoplay on selection ({})", self.key_of(Action::Autoplay))).clicked() {
                self.config.autoplay = !self.config.autoplay;
                let _ = self.config.save();
            }
            if icon_btn(ui, icons::EAR, self.preview).on_hover_text(format!("Preview export: hear and see the card's channels and level ({})", self.key_of(Action::Preview))).clicked() {
                self.preview = !self.preview;
            }
            let t = playhead.unwrap_or(0.0);
            ui.add_sized(egui::vec2(52.0, CONTROL_H), egui::Label::new(RichText::new(waveform::fmt_time(t)).monospace().color(if playing { self.theme.text } else { self.theme.muted })));
            group(ui);
            ui.label(RichText::new("Sel").color(self.theme.muted));
            let mut sel = self.wave.selection.unwrap_or((0.0, 0.0));
            let had = self.wave.selection.is_some();
            let a = ui.add(secs_field(&mut sel.0, duration));
            let b = ui.add(secs_field(&mut sel.1, duration));
            self.fields_dragging = a.dragged() || b.dragged();
            if (a.changed() || b.changed()) && (had || sel.1 > sel.0) {
                self.wave.selection = if sel.1 > sel.0 { Some(sel) } else { None };
            }
            if icon_btn(ui, icons::CARET_LEFT, false).on_hover_text(format!("Previous take ({})", self.key_of(Action::PreviousTake))).clicked() {
                self.step_take(-1);
            }
            if icon_btn(ui, icons::CARET_RIGHT, false).on_hover_text(format!("Next take ({})", self.key_of(Action::NextTake))).clicked() {
                self.step_take(1);
            }
            if icon_btn(ui, icons::ARROWS_IN_LINE_HORIZONTAL, false).on_hover_text(format!("Trim the selection to the sound ({}): what sits {} dB under its peak goes", self.key_of(Action::Trim), hark_core::pcm::TRIM_BELOW_PEAK_DB)).clicked() {
                self.trim_selection();
            }
            if icon_btn(ui, icons::MAGNIFYING_GLASS_PLUS, false).on_hover_text(format!("Zoom to selection ({})", self.key_of(Action::Zoom))).clicked() {
                self.wave.zoom_to_selection(duration);
            }
            if icon_btn(ui, icons::X, false).on_hover_text(format!("Clear selection ({})", self.key_of(Action::ClearSelection))).clicked() {
                self.wave.selection = None;
            }
            group(ui);
            ui.label(RichText::new("Fade").color(self.theme.muted));
            let fade_in = ui.add(secs_field(&mut self.wave.fade_in, duration));
            if fade_in.changed() {
                self.wave.touch_fade(waveform::Fade::In);
            }
            let fade_out = ui.add(secs_field(&mut self.wave.fade_out, duration));
            if fade_out.changed() {
                self.wave.touch_fade(waveform::Fade::Out);
            }
            self.fields_dragging |= fade_in.dragged() || fade_out.dragged();
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
            if icon_btn(ui, icons::X, false).on_hover_text("Clear fades").clicked() {
                self.wave.fade_in = 0.0;
                self.wave.fade_out = 0.0;
            }
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
        // Mono turned on folds as the preset the card came from does, or by average.
        let law = self.preset_name.as_deref().and_then(|n| self.config.preset(n)).and_then(|p| p.mono).unwrap_or_default();
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
            edited |= preset_ui::mono(ui, p, law);
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

        // The button sits at the foot of the card whatever height the panel was dragged to;
        // the card's natural height, which the panel cannot go under, is the content plus it.
        // The cursor measures the content: the card's min rect is stretched to the panel.
        const BUTTON_H: f32 = 40.0;
        let natural = ui.cursor().top() - ui.min_rect().top() + 10.0 + BUTTON_H;
        if (natural - self.export_card_h).abs() > 0.5 {
            self.export_card_h = natural;
            ui.ctx().request_repaint();
        }
        // The button reports its own outcome by colour: ok for a moment after a write, the
        // error colour after a failure, then back to the accent; the footer has the words.
        let can = matches!(req, Some(Ok(_)));
        // How far back toward the accent the fill is, or `None` once the outcome is over.
        let settled = |at: &Instant, secs: f32| {
            let t = at.elapsed().as_secs_f32() / secs;
            (t < 1.0).then(|| 1.0 - waveform::flash_left(t))
        };
        let expired = match &self.export_state {
            ExportState::Done { at, .. } => settled(at, EXPORT_DONE_SECS).is_none(),
            ExportState::Failed { at } => settled(at, EXPORT_FAILED_SECS).is_none(),
            _ => false,
        };
        if expired {
            self.export_state = ExportState::Idle;
        }
        let (fill, text, enabled) = match &self.export_state {
            ExportState::Writing { .. } => (theme.overlay, theme.muted, false),
            ExportState::Done { at, .. } if let Some(back) = settled(at, EXPORT_DONE_SECS) => (theme.ok.lerp_to_gamma(theme.accent, back), theme.on_accent(), can),
            ExportState::Failed { at } if let Some(back) = settled(at, EXPORT_FAILED_SECS) => (theme.err.lerp_to_gamma(theme.accent, back), theme.on_accent(), can),
            _ => (if can { theme.accent } else { theme.overlay }, if can { theme.on_accent() } else { theme.muted }, can),
        };
        if !matches!(self.export_state, ExportState::Idle) {
            ui.ctx().request_repaint();
        }
        ui.with_layout(egui::Layout::bottom_up(egui::Align::Center), |ui| {
            let avail = ui.available_rect_before_wrap();
            let rect = egui::Rect::from_min_max(egui::pos2(avail.left(), avail.bottom() - BUTTON_H), avail.right_bottom());
            let hot = enabled && ui.rect_contains_pointer(rect);
            let fill = if hot { fill.gamma_multiply(1.15).to_opaque() } else { fill };
            let button = egui::Button::new(theme::icon_text(icons::EXPORT, "Export", 15.0, egui::FontFamily::Name(theme::SEMIBOLD.into()), text))
                .fill(fill)
                .stroke(Stroke::NONE)
                .corner_radius(CornerRadius::same(6))
                .min_size(rect.size());
            if ui.add_enabled(enabled, button).on_hover_text(self.key_of(Action::Export)).clicked() {
                self.export();
            }
        });
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
        self.remember_card();
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
        self.restore_card();
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
                let pair = |a: Action, b: Action| format!("{} {}", self.key_of(a), self.key_of(b));
                let hints = [
                    (self.key_of(Action::Play), "Play"),
                    ("↑ ↓".to_string(), "Browse"),
                    (pair(Action::PreviousTake, Action::NextTake), "Takes"),
                    (self.key_of(Action::Favorite), "Favorite"),
                    ("1–9".to_string(), "Tags"),
                    (pair(Action::SelectionStart, Action::SelectionEnd), "Selection"),
                    (self.key_of(Action::Preview), "Preview"),
                    (self.key_of(Action::Export), "Export"),
                    (format!("{}+C", keys::COMMAND), "Copy"),
                    (format!("{}+Drag", keys::DRAG_COPY), "Drag out"),
                ];
                for (key, action) in &hints {
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
                if let Some(phase) = self.scanning {
                    ui.spinner();
                    let text = match phase {
                        ScanPhase::Walking { seen: 0 } => "Scanning".to_string(),
                        ScanPhase::Walking { seen } => format!("Scanning  ·  {seen} files"),
                        ScanPhase::Matching { hashed } => format!("Matching marks  ·  {hashed} files"),
                    };
                    ui.label(RichText::new(text).color(self.theme.muted).size(12.0));
                } else {
                    ui.label(RichText::new(format!("{} files", self.file_count)).color(self.theme.muted).size(12.0));
                }
            });
        });
    }
}

/// A tag as a rounded chip in its colour: filled when `on`, outlined when not.
fn chip(ui: &mut egui::Ui, theme: &Theme, text: &str, color: Color32, on: bool, clickable: bool) -> egui::Response {
    let font = FontId::new(12.0, egui::FontFamily::Name(theme::MEDIUM.into()));
    let galley = ui.painter().layout_no_wrap(text.to_string(), font, Color32::PLACEHOLDER);
    let pad = egui::vec2(8.0, 3.0);
    let size = galley.size() + pad * 2.0;
    let (rect, resp) = ui.allocate_exact_size(size, if clickable { Sense::click() } else { Sense::hover() });
    let hovered = clickable && resp.hovered();
    let (fill, stroke, fg) = if on {
        (color.gamma_multiply(if hovered { 0.45 } else { 0.32 }), Stroke::new(1.0, color.gamma_multiply(0.9)), color.gamma_multiply(1.6).to_opaque())
    } else {
        (if hovered { theme.select } else { Color32::TRANSPARENT }, Stroke::new(1.0, color.gamma_multiply(if hovered { 0.9 } else { 0.45 })), color.gamma_multiply(if hovered { 1.4 } else { 1.0 }).to_opaque())
    };
    let p = ui.painter();
    p.rect_filled(rect, CornerRadius::same(10), fill);
    p.rect_stroke(rect, CornerRadius::same(10), stroke, StrokeKind::Inside);
    p.galley(rect.min + pad, galley, fg);
    resp
}

fn secs_field(v: &mut f64, max: f64) -> egui::DragValue<'_> {
    egui::DragValue::new(v).speed(0.01).range(0.0..=max).fixed_decimals(3)
}

enum ExportState {
    Idle,
    /// `span` of `source`, or all of it.
    Writing { source: PathBuf, span: Option<(f64, f64)> },
    Done { at: Instant },
    Failed { at: Instant },
}

/// How long the export button shows its outcome: a write as long as the wave's flash, a
/// failure twice that.
const EXPORT_DONE_SECS: f32 = waveform::FLASH_SECS;
const EXPORT_FAILED_SECS: f32 = 2.0 * waveform::FLASH_SECS;

/// The wave's right-click menu.
enum WaveAction {
    Copy,
    Trim,
    Zoom,
    Clear,
}

enum FileAction {
    Favorite,
    Tag(String),
    /// A tag typed into the menu, to create if it is new and put on the file.
    NewTag(String),
    Reveal,
    CopyPath,
}

/// The per-file menu: favorite, every tag with the carried ones checked, a field that creates
/// a tag and puts it on the file, and the file's place on disk. Returns what was clicked;
/// toggles keep the menu open, the rest close it. `new_tag` holds the field's text.
fn file_menu(ui: &mut egui::Ui, file: &FileRecord, defs: &[hark_core::TagDef], theme: &Theme, favorite_key: &str, new_tag: &mut String) -> Option<FileAction> {
    theme::menu_items(ui);
    let mut action = None;
    let mut fav = file.favorite;
    if theme::menu_check(ui, &mut fav, theme::icon_text(icons::STAR, "Favorite", 14.0, egui::FontFamily::Proportional, Color32::PLACEHOLDER)).on_hover_text(favorite_key).changed() {
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
    let field = ui.add(egui::TextEdit::singleline(new_tag).hint_text("New tag").desired_width(150.0).margin(egui::vec2(theme::MENU_INSET, 2.0)));
    let field = field.on_hover_text("Enter creates the tag and puts it on this file");
    if field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) && !new_tag.trim().is_empty() {
        action = Some(FileAction::NewTag(std::mem::take(new_tag).trim().to_string()));
        field.request_focus();
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

/// A folder path in a cell; when it is too wide, leading components give way to an ellipsis
/// so the nearest folders stay readable.
fn folder_label(ui: &mut egui::Ui, folder: &Path, color: Color32) {
    let parts: Vec<String> = folder.iter().map(|c| c.to_string_lossy().into_owned()).collect();
    if parts.is_empty() {
        return;
    }
    let sep = std::path::MAIN_SEPARATOR_STR;
    let full = parts.join(sep);
    let font = egui::TextStyle::Body.resolve(ui.style());
    let width = ui.available_width();
    let fits = |ui: &egui::Ui, s: &str| ui.painter().layout_no_wrap(s.to_string(), font.clone(), color).size().x <= width;
    let text = if fits(ui, &full) {
        full.clone()
    } else {
        (1..parts.len()).map(|drop| format!("…{sep}{}", parts[drop..].join(sep))).find(|s| fits(ui, s)).unwrap_or_else(|| "…".to_string())
    };
    let label = ui.label(RichText::new(&text).color(color));
    if text != full {
        label.on_hover_text(full);
    }
}

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

/// Opens a folder in the desktop's file manager, or a file's folder with the file selected
/// where that is supported.
fn reveal(path: &Path) -> std::io::Result<()> {
    use std::process::Command;
    let dir = path.is_dir();
    #[cfg(target_os = "windows")]
    {
        if dir {
            Command::new("explorer").arg(path).spawn()?;
        } else {
            Command::new("explorer").arg(format!("/select,{}", path.display())).spawn()?;
        }
    }
    #[cfg(target_os = "macos")]
    {
        if dir {
            Command::new("open").arg(path).spawn()?;
        } else {
            Command::new("open").arg("-R").arg(path).spawn()?;
        }
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let folder = if dir { path } else { path.parent().unwrap_or(path) };
        Command::new("xdg-open").arg(folder).spawn()?;
    }
    Ok(())
}

/// Two lines: what the file becomes, and what is done to the signal on the way.
impl eframe::App for App {
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        if std::mem::take(&mut self.drag_took_release) {
            let (pos, modifiers) = ctx.input(|i| (i.pointer.interact_pos(), i.modifiers));
            raw_input.events.insert(0, egui::Event::PointerButton { pos: pos.unwrap_or(egui::Pos2::ZERO), button: egui::PointerButton::Primary, pressed: false, modifiers });
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.remember_card();
        let ctx = ui.ctx().clone();
        let zoom = ctx.zoom_factor();
        if (zoom - self.config.ui_zoom).abs() > 1e-3 {
            self.config.ui_zoom = zoom;
            let _ = self.config.save();
        }
        self.handle_done();
        // Hyprland aims a new drag at the window holding the pointer, which during the press
        // is this one, until the pointer moves: a drag that starts under a still pointer and
        // is let go there drops nowhere.
        if self.drag.take_missed() {
            self.set_status("The drop did not land anywhere; drag again to take it", false);
        }
        self.expire_status(&ctx);
        if self.list_dirty {
            self.refresh_list();
        }
        self.handle_keys(&ctx);
        self.request_preview();
        self.sync_clip();

        // A panel forgets its dragged size when its state goes, and the list its column widths.
        if std::mem::take(&mut self.reset_layout) {
            ctx.data_mut(|d| {
                for id in PANELS {
                    d.remove::<egui::containers::panel::PanelState>(egui::Id::new(id));
                }
                d.remove::<ColumnWidths>(egui::Id::new(COLUMNS_ID));
            });
        }

        let pad = theme::PAD;
        let bar = egui::Frame::new().fill(self.theme.bg_deep).inner_margin(Margin::symmetric(pad, 8));
        egui::Panel::top("top").resizable(false).exact_size(TOP_BAR_H + 16.0).frame(bar).show(ui, |ui| {
            self.ui_top(ui);
        });
        egui::Panel::bottom("status").frame(egui::Frame::new().fill(self.theme.bg_deep).inner_margin(Margin::symmetric(pad, 4))).show(ui, |ui| {
            self.ui_status(ui);
        });
        // The export card's natural height is the least the bottom panel can be; dragging it
        // taller gives the waveform the room.
        let bottom_min = self.export_card_h + 2.0 * (theme::CARD_PAD + pad) as f32;
        let bottom_max = (ui.available_rect_before_wrap().height() - LIST_MIN_H).max(bottom_min);
        egui::Panel::bottom("bottom").resizable(true).default_size(bottom_min).size_range(bottom_min..=bottom_max).frame(egui::Frame::new().fill(self.theme.bg).inner_margin(Margin::same(pad))).show(ui, |ui| {
            self.ui_bottom(ui);
        });
        let tree_max = (ui.available_width() * 0.5).max(160.0);
        egui::Panel::left("tree").resizable(true).default_size(TREE_W).size_range(160.0..=tree_max).frame(egui::Frame::new().fill(self.theme.bg_deep).inner_margin(Margin { left: 4, right: 4, top: 8, bottom: 8 })).show(ui, |ui| {
            self.ui_tree(ui);
        });
        egui::CentralPanel::default().frame(egui::Frame::new().fill(self.theme.bg).inner_margin(Margin { left: pad, right: pad, top: 8, bottom: 8 })).show(ui, |ui| {
            self.ui_list(ui);
        });
        // A double-click on a separator puts that panel back to its default size. The handle's
        // id is egui's own naming, `<panel id>.__resize`, which it does not expose.
        for id in PANELS {
            let panel = egui::Id::new(id);
            if ui.read_response(panel.with("__resize")).is_some_and(|r| r.double_clicked()) {
                ctx.data_mut(|d| d.remove::<egui::containers::panel::PanelState>(panel));
            }
        }

        self.ui_settings(&ctx);
        self.hold_drag(&ctx);

        let analysing = self.current.as_ref().map(|c| c.peaks.is_none()).unwrap_or(false);
        if self.is_playing() || !self.pending.is_empty() || analysing {
            ctx.request_repaint_after(Duration::from_millis(16));
        }
    }
}

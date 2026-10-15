#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod theme;
mod waveform;
mod worker;

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use egui::{Color32, CornerRadius, FontId, Key, Margin, RichText, Sense, Stroke, StrokeKind};
use egui_extras::{Column, TableBuilder};
use egui_phosphor::regular as icons;
use hark_core::audition::{Clip, Player};
use hark_core::export::{ExportRequest, Selection};
use hark_core::library::{ChannelClass, Query};
use hark_core::{Config, Db, FileRecord};

use theme::Theme;
use waveform::{WaveEvent, WaveState};
use worker::{Done, Job, Loaded, Worker};

const CACHE_FRAMES: usize = 48_000 * 60 * 20;
const PRELOAD: usize = 3;
const EXPORT_CARD_W: f32 = 330.0;
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

fn main() -> eframe::Result {
    let mut open: Option<PathBuf> = None;
    let mut settings = false;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--open" => open = args.next().map(PathBuf::from),
            "--settings" => settings = true,
            "-h" | "--help" => {
                println!("hark [--open <audio file or folder>] [--settings]");
                return Ok(());
            }
            other => open = Some(PathBuf::from(other)),
        }
    }
    let open = open.and_then(|p| std::fs::canonicalize(p).ok());
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_title("hark").with_app_id("hark").with_inner_size([1500.0, 900.0]).with_min_inner_size([900.0, 600.0]),
        ..Default::default()
    };
    eframe::run_native(
        "hark",
        options,
        Box::new(move |cc| {
            let mut app = App::new(cc);
            if let Some(p) = open {
                app.open_path(&p);
            }
            app.settings_open = settings;
            Ok(Box::new(app))
        }),
    )
}

enum TagAction {
    Color(usize, String),
    Preset(usize, Option<String>),
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

    autoplay: bool,
    loop_on: bool,
    transport_h: f32,
    gain_db: f32,
    preset: String,
    overwrite: bool,
    status: String,
    status_is_error: bool,
    last_export: Option<Result<String, String>>,
    scanning: bool,
    settings_open: bool,
    search_id: egui::Id,
    tag_edit: Option<(usize, String)>,
    project_edit: Option<String>,
    file_count: usize,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let config = Config::load().unwrap_or_else(|e| {
            eprintln!("config: {e:#}");
            Config::default()
        });
        let theme = config.theme_file.as_ref().and_then(|p| Theme::from_file(p)).unwrap_or_default();
        theme.apply(&cc.egui_ctx);
        cc.egui_ctx.set_zoom_factor(config.ui_zoom.clamp(0.5, 3.0));
        let mut db = Db::open().expect("library database");
        db.set_project(&config.active_project);
        let (player, audio_error) = match Player::open(config.audition_sample_rate) {
            Ok(p) => (Some(p), None),
            Err(e) => (None, Some(format!("{e:#}"))),
        };
        let worker = Worker::start(cc.egui_ctx.clone(), 3);
        if !config.roots.is_empty() {
            worker.send(Job::Scan(config.roots.clone()));
        }
        let preset = config.default_preset.clone();
        let scanning = !config.roots.is_empty();
        let expanded = config.scope_folders().into_iter().map(|(_, p)| p).collect();
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
            autoplay: true,
            loop_on: false,
            transport_h: 34.0,
            gain_db: 0.0,
            preset,
            overwrite: false,
            status: String::new(),
            status_is_error: false,
            last_export: None,
            scanning,
            file_count: 0,
            settings_open: false,
            search_id: egui::Id::new("search"),
            tag_edit: None,
            project_edit: None,
        }
    }

    /// Shows a folder, or a file's folder with the file selected, expanding the tree to it.
    fn open_path(&mut self, path: &Path) {
        let dir = if path.is_dir() { path.to_path_buf() } else { path.parent().map(|p| p.to_path_buf()).unwrap_or_default() };
        let mut p = dir.clone();
        loop {
            self.expanded.insert(p.clone());
            match p.parent() {
                Some(parent) if self.config.roots.iter().any(|r| p != r.path) && parent != Path::new("/") => p = parent.to_path_buf(),
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

    fn set_status(&mut self, msg: impl Into<String>, error: bool) {
        self.status = msg.into();
        self.status_is_error = error;
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
            q.scope = self.config.project().scope.clone();
            q.limit = SEARCH_LIMIT;
        } else {
            match &self.location {
                Some(Location::Folder(d)) => q.dir = Some(d.clone()),
                Some(Location::Favorites) => {
                    q.favorites_only = true;
                    q.scope = self.config.project().scope.clone();
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
                self.set_status(format!("library: {e:#}"), true);
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
        self.worker.send(Job::Load { path: path.to_path_buf(), device_rate: self.device_rate() });
    }

    fn remember(&mut self, loaded: Arc<Loaded>) {
        let path = loaded.path.clone();
        self.cache_order.retain(|p| p != &path);
        self.cache_order.push_back(path.clone());
        self.cache.insert(path, loaded);
        let mut frames: usize = self.cache.values().map(|l| l.device.frames()).sum();
        while frames > CACHE_FRAMES && self.cache_order.len() > 1 {
            let Some(old) = self.cache_order.pop_front() else { break };
            if self.current.as_ref().map(|c| c.path == old).unwrap_or(false) {
                self.cache_order.push_back(old);
                if self.cache_order.len() == 1 {
                    break;
                }
                continue;
            }
            if let Some(l) = self.cache.remove(&old) {
                frames -= l.device.frames();
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
        self.current = Some(l);
    }

    fn play(&mut self, from_secs: Option<f64>) {
        let (Some(player), Some(cur)) = (&self.player, &self.current) else { return };
        let rate = cur.device.sample_rate as f64;
        let total = cur.device.frames();
        let (start, end) = match self.wave.selection {
            Some((a, b)) => ((a * rate) as usize, ((b * rate) as usize).min(total)),
            None => (0, total),
        };
        let start = match from_secs {
            Some(t) => ((t * rate) as usize).clamp(0, total),
            None => start,
        };
        let outside = from_secs.is_some() && self.wave.selection.map(|(a, b)| start < (a * rate) as usize || start >= (b * rate) as usize).unwrap_or(true);
        let end = if outside { total } else { end };
        let in_selection = self.wave.selection.is_some() && !outside;
        let sel_start = self.wave.selection.map(|(a, _)| (a * rate) as usize).unwrap_or(0);
        let (fade_in, fade_out) = if in_selection { ((self.wave.fade_in * rate) as usize, (self.wave.fade_out * rate) as usize) } else { (0, 0) };
        player.play(Clip {
            pcm: cur.device.clone(),
            start,
            end: end.max(start),
            looped: self.loop_on,
            fade_in: if in_selection && start > sel_start { 0 } else { fade_in },
            fade_out,
            fade_shape: self.wave.fade_shape,
        });
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
            Err(e) => self.set_status(format!("favorite: {e:#}"), true),
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
            Err(e) => self.set_status(format!("tag: {e:#}"), true),
        }
    }


    fn export_request(&self) -> Option<Result<ExportRequest, String>> {
        let i = self.selected?;
        let f = &self.files[i];
        let mut req = match ExportRequest::from_config(&self.config, &f.path, Some(&self.preset), f.tags.clone(), f.favorite) {
            Ok(r) => r,
            Err(e) => return Some(Err(format!("{e:#}"))),
        };
        req.selection = self.wave.selection.map(|(a, b)| Selection { start_secs: a, end_secs: b });
        req.fade_in_secs = self.wave.fade_in;
        req.fade_out_secs = self.wave.fade_out;
        req.preset.fade_shape = self.wave.fade_shape;
        req.overwrite = self.overwrite;
        Some(Ok(req))
    }

    fn export(&mut self) {
        match self.export_request() {
            Some(Ok(req)) => {
                self.last_export = None;
                self.set_status(format!("exporting {}", req.output_path().display()), false);
                self.worker.send(Job::Export(req));
            }
            Some(Err(e)) => self.set_status(format!("export: {e}"), true),
            None => {}
        }
    }

    fn handle_done(&mut self) {
        for done in self.worker.poll() {
            match done {
                Done::Loaded(path, result) => {
                    self.pending.remove(&path);
                    match result {
                        Ok(l) => {
                            if let Some(ld) = l.loudness {
                                let _ = self.db.set_loudness(&path, ld.integrated_lufs, ld.true_peak_dbfs);
                                if let Some(i) = self.files.iter().position(|f| f.path == path) {
                                    self.files[i].lufs = Some(ld.integrated_lufs);
                                    self.files[i].true_peak_dbfs = Some(ld.true_peak_dbfs);
                                }
                            }
                            self.remember(l.clone());
                            let wanted = self.selected.and_then(|i| self.files.get(i)).map(|f| f.path == path).unwrap_or(false);
                            if wanted && self.current.as_ref().map(|c| c.path != path).unwrap_or(true) {
                                self.show_loaded(l);
                                if self.autoplay {
                                    self.play(None);
                                }
                            }
                        }
                        Err(e) => self.set_status(format!("{}: {e}", path.display()), true),
                    }
                }
                Done::Exported(result) => match result {
                    Ok(r) => {
                        let mut msg = format!("{}  ·  {:+.2} dB", r.output.display(), r.origin.measured.applied_gain_db);
                        for w in &r.warnings {
                            msg.push_str("\n");
                            msg.push_str(w);
                        }
                        self.set_status(format!("exported {}", r.output.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()), false);
                        self.last_export = Some(Ok(msg));
                    }
                    Err(e) => {
                        self.set_status("export failed", true);
                        self.last_export = Some(Err(e));
                    }
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
        if pressed(Key::Delete) || pressed(Key::Backspace) {
            self.wave.selection = None;
            self.wave.fade_in = 0.0;
            self.wave.fade_out = 0.0;
        }
        if let Some(cur) = &self.current {
            let pos = self.player.as_ref().map(|p| p.position() as f64 / cur.device.sample_rate as f64);
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
        ui.add(egui::Button::new(theme::icon_text(icon, label, 14.0, egui::FontFamily::Proportional, Color32::PLACEHOLDER)).selected(on))
    }

    // ---- panels ----

    fn ui_top(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = theme::GAP;
            ui.label(theme::semibold("hark").size(18.0).color(self.theme.accent));
            let mut switch: Option<String> = None;
            egui::ComboBox::from_id_salt("project")
                .selected_text(theme::icon_text(icons::CUBE, &self.config.active_project, 14.0, egui::FontFamily::Proportional, Color32::PLACEHOLDER))
                .show_ui(ui, |ui| {
                    for p in self.config.projects.iter().map(|p| p.name.clone()).collect::<Vec<_>>() {
                        if ui.selectable_label(p == self.config.active_project, &p).clicked() {
                            switch = Some(p);
                        }
                    }
                    ui.separator();
                    if ui.selectable_label(false, theme::icon_text(icons::PLUS, "new project", 14.0, egui::FontFamily::Proportional, Color32::PLACEHOLDER)).clicked() {
                        switch = Some(String::new());
                    }
                });
            if let Some(p) = switch {
                if p.is_empty() {
                    self.new_project();
                } else {
                    self.switch_project(&p);
                }
            }
            let focused = ui.memory(|m| m.has_focus(self.search_id));
            let box_frame = egui::Frame::new()
                .fill(self.theme.surface)
                .stroke(Stroke::new(1.0, if focused { self.theme.accent } else { self.theme.line }))
                .corner_radius(CornerRadius::same(6))
                .inner_margin(Margin::symmetric(8, 4));
            box_frame.show(ui, |ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.label(theme::icon(icons::MAGNIFYING_GLASS).color(self.theme.muted));
                let search = ui.add(egui::TextEdit::singleline(&mut self.search).id(self.search_id).frame(egui::Frame::NONE).hint_text("search the library").desired_width(240.0));
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
            self.ui_filter(ui);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if Self::icon_button(ui, icons::GEAR, "", self.settings_open).on_hover_text("settings").clicked() {
                    self.settings_open = !self.settings_open;
                }
            });
        });
    }

    fn add_root(&mut self) {
        let Some(path) = rfd::FileDialog::new().pick_folder() else { return };
        if self.config.roots.iter().any(|r| r.path == path) {
            return;
        }
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "root".into());
        self.config.roots.push(hark_core::config::Root { name, path: path.clone() });
        let _ = self.config.save();
        self.expanded.insert(path);
        self.rescan();
    }

    fn rescan(&mut self) {
        self.scanning = true;
        self.dir_children.clear();
        self.worker.send(Job::Scan(self.config.roots.clone()));
    }

    fn ui_filter(&mut self, ui: &mut egui::Ui) {
        let n = self.filter.count();
        let label = if n == 0 { "filter".to_string() } else { format!("filter · {n}") };
        let button = Self::icon_button(ui, icons::FUNNEL, &label, n > 0);
        let muted = self.theme.muted;
        let heading = |ui: &mut egui::Ui, text: &str| {
            ui.add_space(4.0);
            ui.label(RichText::new(text).color(muted).size(12.0));
        };
        let mut changed = false;
        egui::Popup::menu(&button).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).width(200.0).show(|ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            changed |= ui.checkbox(&mut self.filter.favorites, theme::icon_text(icons::STAR, "favorites", 14.0, egui::FontFamily::Proportional, Color32::PLACEHOLDER)).changed();
            heading(ui, "tags");
            for (k, t) in self.config.tags.clone().iter().enumerate() {
                let mut on = self.filter.tags.contains(&t.name);
                if ui.checkbox(&mut on, RichText::new(&t.name).color(self.theme.color_of(k, Some(t)).gamma_multiply(1.3).to_opaque())).changed() {
                    toggle_in(&mut self.filter.tags, t.name.clone());
                    changed = true;
                }
            }
            heading(ui, "file type");
            for (name, _) in FILE_TYPES {
                let mut on = self.filter.types.contains(&name);
                if ui.checkbox(&mut on, name).changed() {
                    toggle_in(&mut self.filter.types, name);
                    changed = true;
                }
            }
            heading(ui, "channels");
            for c in ChannelClass::ALL {
                let mut on = self.filter.channels.contains(&c);
                if ui.checkbox(&mut on, c.name()).changed() {
                    toggle_in(&mut self.filter.channels, c);
                    changed = true;
                }
            }
            if n > 0 {
                ui.add_space(4.0);
                if ui.button("clear").clicked() {
                    self.filter.clear();
                    changed = true;
                }
            }
        });
        if changed {
            self.list_dirty = true;
        }
    }

    fn ui_tree(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.label(RichText::new("Library").color(self.theme.muted).size(12.0));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                ui.add_space(4.0);
                if ui.add_enabled(!self.scanning && !self.config.roots.is_empty(), egui::Button::new(theme::icon(icons::ARROWS_CLOCKWISE)).small()).on_hover_text("rescan the library").clicked() {
                    self.rescan();
                }
                if ui.add(egui::Button::new(theme::icon(icons::FOLDER_PLUS)).small()).on_hover_text("add a library folder").clicked() {
                    self.add_root();
                }
            });
        });
        ui.add_space(4.0);
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            let selected = self.location == Some(Location::Favorites);
            let count = if self.fav_count > 0 { Some(self.fav_count.to_string()) } else { None };
            let (row, _) = self.tree_row(ui, 0, None, (icons::STAR, theme::ICONS_FILL), self.theme.accent2, "Favorites", selected, count);
            if row.clicked() {
                self.location = Some(Location::Favorites);
                self.search.clear();
                self.list_dirty = true;
            }
            ui.add_space(6.0);
            if self.config.roots.is_empty() {
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.add_space(8.0);
                    ui.weak("No library folders yet. Add one with the folder button above.");
                });
                return;
            }
            for (name, path) in self.config.scope_folders() {
                self.ui_tree_node(ui, &path, &name, 0);
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
            self.theme.accent_soft
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
            if n >= SEARCH_LIMIT { format!("first {n} results") } else { format!("{n} results") }
        } else {
            match &self.location {
                Some(Location::Folder(d)) => {
                    let shown = self.config.root_of(d).and_then(|r| d.strip_prefix(&r.path).ok()).map(|rel| if rel.as_os_str().is_empty() { String::new() } else { format!("  /  {}", rel.display()) });
                    match (self.config.root_of(d), shown) {
                        (Some(r), Some(rel)) => format!("{}{rel}   ·   {n} files", r.name),
                        _ => format!("{}   ·   {n} files", d.display()),
                    }
                }
                Some(Location::Favorites) => format!("Favorites of {}   ·   {n} files", self.config.active_project),
                None => "pick a folder".to_string(),
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
                for title in ["", "name", "tags", "length", "ch", "rate", "bits", "codec", "LUFS"] {
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
                            (true, true) => theme.accent2.gamma_multiply(1.4).to_opaque(),
                            (true, false) | (false, true) => theme.accent2,
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
                    self.set_status(format!("show in folder: {e}"), true);
                }
            }
            FileAction::CopyPath => {
                let p = f.path.display().to_string();
                self.set_status(format!("copied {p}"), false);
            }
        }
    }

    fn ui_bottom(&mut self, ui: &mut egui::Ui) {
        egui::Panel::right("export").resizable(false).default_size(EXPORT_CARD_W).frame(egui::Frame::new().inner_margin(Margin { left: theme::PAD, right: 0, top: 0, bottom: 0 })).show(ui, |ui| {
            self.ui_export_card(ui);
        });
        egui::CentralPanel::default().frame(egui::Frame::new().inner_margin(Margin::ZERO)).show(ui, |ui| {
            self.ui_wave_column(ui);
        });
    }

    fn ui_wave_column(&mut self, ui: &mut egui::Ui) {
        let playing = self.is_playing();
        let playhead = match (&self.player, &self.current) {
            (Some(p), Some(c)) if playing => Some(p.position() as f64 / c.device.sample_rate as f64),
            _ => None,
        };
        let duration = self.current.as_ref().map(|c| c.duration_secs).unwrap_or(0.0);

        // The file's tag chips and the tag button sit at the right of the header when they fit
        // beside the name, else on a row of their own.
        let carried: Vec<String> = self.selected.map(|i| self.files[i].tags.clone()).unwrap_or_default();
        let chips_w = carried.iter().map(|t| self.chip_width(ui, t) + 4.0).sum::<f32>() + 40.0 + theme::GAP;
        let mut chips_below = false;
        ui.horizontal(|ui| {
            if let Some(c) = self.current.clone() {
                let name = c.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                ui.label(theme::semibold(name).size(15.0));
                let mut meta = format!("{} Hz  ·  {} ch  ·  {}", c.source_rate, c.channels, waveform::fmt_time(c.duration_secs));
                if let Some(l) = c.loudness {
                    meta.push_str(&format!("  ·  {:.1} LUFS{}  ·  {:.1} dBTP", l.integrated_lufs, if l.momentary { " (M)" } else { "" }, l.true_peak_dbfs));
                }
                ui.add(egui::Label::new(RichText::new(meta).color(self.theme.muted)).truncate());
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
                    ui.label(RichText::new("loading").color(self.theme.muted));
                }
            } else {
                ui.label(RichText::new("no file").color(self.theme.muted));
            }
        });
        if chips_below {
            ui.horizontal_wrapped(|ui| self.tag_chips(ui, false));
        }

        ui.add_space(2.0);
        // The transport row wraps at narrow widths; the waveform leaves it the height it took on
        // the last frame.
        let wave_h = (ui.available_height() - self.transport_h - 10.0).max(80.0);
        let peaks = self.current.as_ref().map(|c| c.peaks.clone());
        let samples = self.current.as_ref().map(|c| c.device.clone());
        let colors = waveform::WaveColors {
            peak: self.theme.accent.gamma_multiply(0.55),
            rms: self.theme.accent.gamma_multiply(0.95),
            selection: self.theme.accent2,
            fade: self.theme.accent2.gamma_multiply(1.3).to_opaque(),
            playhead: self.theme.err,
            axis: self.theme.line,
        };
        let event = waveform::show(ui, &mut self.wave, peaks.as_deref(), samples.as_deref(), duration, playhead, wave_h, &colors);
        if let WaveEvent::Seek(t) = event {
            self.play(Some(t));
        }
        ui.add_space(4.0);
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
        ui.spacing_mut().item_spacing.x = 4.0;
        let defs = self.config.tags.clone();
        let file = self.files[i].clone();
        let mut action = None;
        let menu_button = |ui: &mut egui::Ui, action: &mut Option<FileAction>| {
            let b = ui.add(egui::Button::new(theme::icon(icons::TAG)).small()).on_hover_text("tags and more (right-click a row)");
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
        // Every control in the row is CONTROL_H tall and keeps the same padding, so the row
        // reads as one line whatever mix of buttons, fields and the slider it holds.
        const CONTROL_H: f32 = 28.0;
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
            if icon_btn(ui, play, false).on_hover_text("play / stop (Space)").clicked() {
                if playing {
                    self.stop();
                } else {
                    self.play(None);
                }
            }
            if icon_btn(ui, icons::REPEAT, self.loop_on).on_hover_text("loop (L)").clicked() {
                self.loop_on = !self.loop_on;
            }
            if ui.add(egui::Button::new("auto").selected(self.autoplay).min_size(egui::vec2(0.0, CONTROL_H))).on_hover_text("autoplay on selection (A)").clicked() {
                self.autoplay = !self.autoplay;
            }
            let t = playhead.unwrap_or(0.0);
            ui.add_sized(egui::vec2(52.0, CONTROL_H), egui::Label::new(RichText::new(waveform::fmt_time(t)).monospace().color(if playing { self.theme.text } else { self.theme.muted })));
            ui.scope(|ui| {
                ui.spacing_mut().slider_width = 90.0;
                ui.add(egui::Slider::new(&mut self.gain_db, -36.0..=12.0).suffix(" dB").show_value(true).trailing_fill(false)).on_hover_text("audition gain");
            });
            if ui.input(|i| i.pointer.any_released()) {
                if let Some(p) = &self.player {
                    p.set_gain(hark_core::pcm::db_to_gain(self.gain_db as f64));
                }
            }
            group(ui);
            ui.label(RichText::new("sel").color(self.theme.muted));
            let mut sel = self.wave.selection.unwrap_or((0.0, 0.0));
            let had = self.wave.selection.is_some();
            let a = ui.add(secs_field(&mut sel.0, duration));
            let b = ui.add(secs_field(&mut sel.1, duration));
            if (a.changed() || b.changed()) && (had || sel.1 > sel.0) {
                self.wave.selection = if sel.1 > sel.0 { Some(sel) } else { None };
            }
            if icon_btn(ui, icons::X, false).on_hover_text("clear selection (Del)").clicked() {
                self.wave.selection = None;
            }
            if icon_btn(ui, icons::MAGNIFYING_GLASS_PLUS, false).on_hover_text("zoom to selection (Z)").clicked() {
                self.wave.zoom_to_selection();
            }
            group(ui);
            ui.label(RichText::new("fade").color(self.theme.muted));
            ui.add(secs_field(&mut self.wave.fade_in, duration));
            ui.add(secs_field(&mut self.wave.fade_out, duration));
            egui::ComboBox::from_id_salt("fade-shape")
                .width(64.0)
                .selected_text(match self.wave.fade_shape {
                    hark_core::pcm::FadeShape::EqualPower => "eq",
                    hark_core::pcm::FadeShape::Linear => "lin",
                    hark_core::pcm::FadeShape::Logarithmic => "log",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.wave.fade_shape, hark_core::pcm::FadeShape::EqualPower, "equal power");
                    ui.selectable_value(&mut self.wave.fade_shape, hark_core::pcm::FadeShape::Linear, "linear");
                    ui.selectable_value(&mut self.wave.fade_shape, hark_core::pcm::FadeShape::Logarithmic, "log");
                });
            self.wave.clamp_fades();
        });
    }

    /// The card fills the panel: preset and target at the top, the result of the last export
    /// when there is one, and the export button taking whatever height is left.
    fn ui_export_card(&mut self, ui: &mut egui::Ui) {
        let theme = self.theme.clone();
        egui::Frame::new().fill(theme.surface).corner_radius(CornerRadius::same(8)).inner_margin(Margin::same(14)).show(ui, |ui| {
            ui.set_min_height(ui.available_height());
            ui.spacing_mut().item_spacing.y = 6.0;
            let small = |text: String, color: Color32| egui::Label::new(RichText::new(text).color(color).size(12.5)).truncate();

            ui.horizontal(|ui| {
                ui.label(theme::semibold("Export").size(15.0));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    egui::ComboBox::from_id_salt("preset").width(150.0).selected_text(&self.preset).show_ui(ui, |ui| {
                        for name in self.config.presets.keys().cloned().collect::<Vec<_>>() {
                            ui.selectable_value(&mut self.preset, name.clone(), name);
                        }
                    });
                });
            });
            if let Some(p) = self.config.preset(&self.preset).cloned() {
                for l in preset_summary(&p) {
                    ui.add(small(l, theme.muted));
                }
            }

            ui.add_space(theme::GAP - 6.0);
            let out = self.config.output_root().map(|p| p.display().to_string());
            let folder = egui::Button::new(theme::icon_text(icons::FOLDER, out.as_deref().unwrap_or("choose an output folder…"), 12.5, egui::FontFamily::Proportional, Color32::PLACEHOLDER)).min_size(egui::vec2(ui.available_width(), 0.0)).truncate();
            let folder = ui.add(folder);
            let folder = match &out {
                Some(o) => folder.on_hover_text(o),
                None => folder,
            };
            if folder.clicked() {
                if let Some(path) = rfd::FileDialog::new().pick_folder() {
                    self.config.project_mut().output_root = Some(path);
                    let _ = self.config.save();
                }
            }
            match self.export_request() {
                Some(Ok(req)) => {
                    let rel = req.output_path();
                    let rel = rel.strip_prefix(&req.output_root).map(|p| p.to_path_buf()).unwrap_or(rel);
                    ui.add(small(format!("→ {}", rel.display()), theme.text));
                    let span = match req.selection {
                        Some(s) => format!("{} – {}", waveform::fmt_time(s.start_secs), waveform::fmt_time(s.end_secs)),
                        None => "whole file".into(),
                    };
                    ui.horizontal(|ui| {
                        ui.add(small(span, theme.muted));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.checkbox(&mut self.overwrite, RichText::new("overwrite").size(12.5));
                        });
                    });
                }
                Some(Err(e)) => {
                    ui.add(small(e, theme.warn));
                }
                None => {
                    ui.add(small("select a file".into(), theme.muted));
                }
            }

            if let Some(result) = &self.last_export {
                ui.add_space(2.0);
                match result {
                    Ok(msg) => {
                        for (k, line) in msg.lines().enumerate() {
                            ui.add(small(line.to_string(), if k == 0 { theme.ok } else { theme.warn }));
                        }
                    }
                    Err(e) => {
                        ui.add(small(e.clone(), theme.err));
                    }
                }
            }

            ui.add_space(theme::GAP - 6.0);
            let can = self.selected.is_some() && self.config.output_root().is_some();
            let button_h = ui.available_height().max(36.0);
            let button = egui::Button::new(theme::icon_text(icons::EXPORT, "Export", 15.0, egui::FontFamily::Name(theme::SEMIBOLD.into()), if can { Color32::WHITE } else { theme.muted }))
                .fill(if can { theme.accent } else { theme.overlay })
                .corner_radius(CornerRadius::same(6))
                .min_size(egui::vec2(ui.available_width(), button_h));
            if ui.add_enabled(can, button).on_hover_text("E").clicked() {
                self.export();
            }
        });
    }

    fn switch_project(&mut self, name: &str) {
        if !self.config.projects.iter().any(|p| p.name == name) {
            return;
        }
        self.config.active_project = name.to_string();
        self.db.set_project(name);
        let _ = self.config.save();
        self.location = None;
        self.list_dirty = true;
        self.expanded = self.config.scope_folders().into_iter().map(|(_, p)| p).collect();
    }

    fn new_project(&mut self) {
        let mut name = "project".to_string();
        let mut n = 2;
        while self.config.projects.iter().any(|p| p.name == name) {
            name = format!("project{n}");
            n += 1;
        }
        self.config.projects.push(hark_core::Project { name: name.clone(), output_root: None, scope: Vec::new() });
        self.switch_project(&name);
        self.project_edit = Some(name);
        self.settings_open = true;
    }

    fn ui_projects_settings(&mut self, ui: &mut egui::Ui) {
        ui.label(theme::semibold("Projects").size(15.0));
        ui.label(RichText::new("A project owns an output folder, a library scope and its own favorites.").color(self.theme.muted).size(12.0));
        ui.add_space(6.0);
        let names: Vec<String> = self.config.projects.iter().map(|p| p.name.clone()).collect();
        ui.horizontal(|ui| {
            for name in &names {
                if ui.selectable_label(name == &self.config.active_project, name).clicked() {
                    self.switch_project(name);
                }
            }
            if ui.add(egui::Button::new(theme::icon(icons::PLUS)).small()).on_hover_text("new project").clicked() {
                self.new_project();
            }
        });
        ui.add_space(6.0);
        let active = self.config.active_project.clone();
        let favs = self.db.favorite_count(&active).unwrap_or(0);
        egui::Grid::new("project-grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
            ui.label(RichText::new("name").color(self.theme.muted));
            let editing = self.project_edit.is_some();
            if editing {
                let buf = self.project_edit.as_mut().unwrap();
                let resp = ui.add(egui::TextEdit::singleline(buf).desired_width(200.0));
                if resp.lost_focus() || ui.input(|i| i.key_pressed(Key::Enter)) {
                    let new = buf.trim().to_string();
                    self.project_edit = None;
                    if !new.is_empty() && new != active && !self.config.projects.iter().any(|p| p.name == new) {
                        if let Err(e) = self.db.rename_project(&active, &new) {
                            self.set_status(format!("rename project: {e:#}"), true);
                        } else {
                            self.config.project_mut().name = new.clone();
                            self.config.active_project = new.clone();
                            self.db.set_project(&new);
                            let _ = self.config.save();
                        }
                    }
                }
            } else if ui.add(egui::Button::new(&active).min_size(egui::vec2(200.0, 0.0))).on_hover_text("click to rename").clicked() {
                self.project_edit = Some(active.clone());
            }
            ui.end_row();
            ui.label(RichText::new("output folder").color(self.theme.muted));
            let out = self.config.output_root().map(|p| p.display().to_string()).unwrap_or_else(|| "choose…".into());
            if ui.add(egui::Button::new(theme::icon_text(icons::FOLDER, &out, 13.0, egui::FontFamily::Proportional, Color32::PLACEHOLDER))).clicked() {
                if let Some(path) = rfd::FileDialog::new().pick_folder() {
                    self.config.project_mut().output_root = Some(path);
                    let _ = self.config.save();
                }
            }
            ui.end_row();
            ui.label(RichText::new("favorites").color(self.theme.muted));
            ui.label(format!("{favs}"));
            ui.end_row();
            ui.label(RichText::new("scope").color(self.theme.muted));
            ui.vertical(|ui| {
                let scope = self.config.project().scope.clone();
                if scope.is_empty() {
                    ui.label(RichText::new("whole library").color(self.theme.muted));
                }
                let mut remove: Option<usize> = None;
                for (i, p) in scope.iter().enumerate() {
                    ui.horizontal(|ui| {
                        if ui.add(egui::Button::new(theme::icon(icons::X)).small()).clicked() {
                            remove = Some(i);
                        }
                        ui.label(p.display().to_string());
                    });
                }
                if let Some(i) = remove {
                    self.config.project_mut().scope.remove(i);
                    let _ = self.config.save();
                    self.expanded = self.config.scope_folders().into_iter().map(|(_, p)| p).collect();
                    self.list_dirty = true;
                }
                if ui.add(egui::Button::new(theme::icon_text(icons::FOLDER_PLUS, "add folder to scope", 13.0, egui::FontFamily::Proportional, Color32::PLACEHOLDER))).clicked() {
                    let start = self.config.roots.first().map(|r| r.path.clone());
                    let mut dialog = rfd::FileDialog::new();
                    if let Some(s) = start {
                        dialog = dialog.set_directory(s);
                    }
                    if let Some(path) = dialog.pick_folder() {
                        if !self.config.project().scope.contains(&path) {
                            self.config.project_mut().scope.push(path.clone());
                            let _ = self.config.save();
                            self.expanded.insert(path);
                            self.list_dirty = true;
                        }
                    }
                }
            });
            ui.end_row();
        });
        if self.config.projects.len() > 1 {
            ui.add_space(4.0);
            if ui.add(egui::Button::new(theme::icon_text(icons::TRASH, "remove this project", 13.0, egui::FontFamily::Proportional, self.theme.err))).clicked() {
                let i = self.config.projects.iter().position(|p| p.name == active).unwrap_or(0);
                self.config.projects.remove(i);
                let _ = self.db.forget_project(&active);
                let next = self.config.projects[0].name.clone();
                self.switch_project(&next);
            }
        }
    }

    fn ui_settings(&mut self, ctx: &egui::Context) {
        if !self.settings_open {
            return;
        }
        // A real window of its own, so the compositor tiles it like any other.
        let builder = egui::ViewportBuilder::default().with_title("hark settings").with_app_id("hark").with_inner_size([640.0, 780.0]).with_min_inner_size([480.0, 400.0]);
        ctx.show_viewport_immediate(egui::ViewportId::from_hash_of("settings"), builder, |ctx, _| {
            if ctx.input(|i| i.viewport().close_requested()) {
                self.settings_open = false;
                self.tag_edit = None;
                self.project_edit = None;
            }
            let frame = egui::Frame::new().fill(self.theme.bg).inner_margin(Margin::same(theme::PAD + 4));
            egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| self.ui_settings_body(ui));
            });
        });
    }

    fn ui_settings_body(&mut self, ui: &mut egui::Ui) {
        self.ui_projects_settings(ui);
        ui.add_space(theme::GAP * 1.5);
        ui.label(theme::semibold("Tags").size(15.0));
        ui.label(RichText::new("Order is the 1 to 9 hotkey order. Renaming rewrites the marks; removing drops them.").color(self.theme.muted).size(12.0));
        ui.add_space(6.0);
        let used: HashMap<String, usize> = self.db.tags_in_use().unwrap_or_default().into_iter().collect();
        let mut action: Option<TagAction> = None;
        let n = self.config.tags.len();
        egui::Grid::new("tags-grid").num_columns(6).spacing([12.0, 8.0]).show(ui, |ui| {
            for k in 0..n {
                let def = self.config.tags[k].clone();
                let color = self.theme.color_of(k, Some(&def));
                ui.label(RichText::new(if k < 9 { format!("{}", k + 1) } else { String::new() }).color(self.theme.muted).monospace());
                let mut c = color;
                if ui.color_edit_button_srgba(&mut c).changed() {
                    action = Some(TagAction::Color(k, theme::to_hex(c)));
                }
                let editing = self.tag_edit.as_ref().map(|(i, _)| *i == k).unwrap_or(false);
                if editing {
                    let (_, buf) = self.tag_edit.as_mut().unwrap();
                    let resp = ui.add(egui::TextEdit::singleline(buf).desired_width(140.0));
                    let done = resp.lost_focus() || ui.input(|i| i.key_pressed(Key::Enter));
                    if done {
                        let new = buf.trim().to_string();
                        action = Some(TagAction::Rename(k, new));
                    }
                } else if ui.add(egui::Button::new(RichText::new(&def.name)).min_size(egui::vec2(140.0, 0.0))).on_hover_text("click to rename").clicked() {
                    self.tag_edit = Some((k, def.name.clone()));
                }
                egui::ComboBox::from_id_salt(("tag-preset", k)).width(110.0).selected_text(def.preset.clone().unwrap_or_else(|| "no preset".into())).show_ui(ui, |ui| {
                    if ui.selectable_label(def.preset.is_none(), "no preset").clicked() {
                        action = Some(TagAction::Preset(k, None));
                    }
                    for name in self.config.presets.keys() {
                        if ui.selectable_label(def.preset.as_deref() == Some(name), name).clicked() {
                            action = Some(TagAction::Preset(k, Some(name.clone())));
                        }
                    }
                });
                let n_used = used.get(&def.name).copied().unwrap_or(0);
                ui.label(RichText::new(format!("{n_used} file{}", if n_used == 1 { "" } else { "s" })).color(self.theme.muted).size(12.0));
                ui.horizontal(|ui| {
                    if ui.add_enabled(k > 0, egui::Button::new(theme::icon(icons::CARET_UP)).small()).clicked() {
                        action = Some(TagAction::Move(k, k - 1));
                    }
                    if ui.add_enabled(k + 1 < n, egui::Button::new(theme::icon(icons::CARET_DOWN)).small()).clicked() {
                        action = Some(TagAction::Move(k, k + 1));
                    }
                    if ui.add(egui::Button::new(theme::icon(icons::TRASH)).small()).on_hover_text("remove the tag and its marks").clicked() {
                        action = Some(TagAction::Remove(k));
                    }
                });
                ui.end_row();
            }
        });
        ui.add_space(4.0);
        if ui.add(egui::Button::new(theme::icon_text(icons::PLUS, "new tag", 14.0, egui::FontFamily::Proportional, Color32::PLACEHOLDER))).clicked() {
            action = Some(TagAction::Add);
        }
        if let Some(a) = action {
            self.apply_tag_action(a);
        }
        ui.add_space(theme::GAP * 1.5);
        ui.label(theme::semibold("Export").size(15.0));
        egui::Grid::new("general-grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
            ui.label(RichText::new("default preset").color(self.theme.muted));
            let mut changed = false;
            egui::ComboBox::from_id_salt("default-preset").selected_text(&self.config.default_preset).show_ui(ui, |ui| {
                for name in self.config.presets.keys().cloned().collect::<Vec<_>>() {
                    if ui.selectable_value(&mut self.config.default_preset, name.clone(), name).clicked() {
                        changed = true;
                    }
                }
            });
            ui.end_row();
            ui.label(RichText::new("ui zoom").color(self.theme.muted));
            let zoom = ui.add(egui::Slider::new(&mut self.config.ui_zoom, 0.8..=2.0).step_by(0.05).fixed_decimals(2)).on_hover_text("Ctrl+= and Ctrl+- also change it");
            if zoom.changed() {
                ui.ctx().set_zoom_factor(self.config.ui_zoom);
            }
            if zoom.drag_stopped() || zoom.lost_focus() {
                changed = true;
            }
            ui.end_row();
            if changed {
                let _ = self.config.save();
            }
        });
    }

    fn apply_tag_action(&mut self, action: TagAction) {
        match action {
            TagAction::Color(k, hex) => self.config.tags[k].color = Some(hex),
            TagAction::Preset(k, p) => self.config.tags[k].preset = p,
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
                        Err(e) => self.set_status(format!("rename tag: {e:#}"), true),
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
                    self.set_status(format!("remove tag: {e:#}"), true);
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
                self.config.tags.push(hark_core::TagDef { name: name.clone(), color: None, preset: None });
                self.tag_edit = Some((self.config.tags.len() - 1, name));
            }
        }
        if let Err(e) = self.config.save() {
            self.set_status(format!("save config: {e:#}"), true);
        }
        self.list_dirty = true;
    }

    fn ui_status(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let color = if self.status_is_error { self.theme.err } else { self.theme.muted };
            ui.label(RichText::new(&self.status).color(color).size(12.0));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let device = match (&self.player, &self.audio_error) {
                    (Some(p), _) => theme::icon_text(icons::SPEAKER_HIGH, &format!("{}  ·  {} Hz", p.device_name, p.sample_rate), 12.0, egui::FontFamily::Proportional, self.theme.muted),
                    (None, Some(e)) => theme::icon_text(icons::SPEAKER_HIGH, &format!("no audio: {e}"), 12.0, egui::FontFamily::Proportional, self.theme.err),
                    _ => theme::icon_text(icons::SPEAKER_HIGH, "no audio", 12.0, egui::FontFamily::Proportional, self.theme.err),
                };
                ui.label(device);
                ui.separator();
                if self.scanning {
                    ui.spinner();
                    ui.label(RichText::new("scanning").color(self.theme.muted).size(12.0));
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
    ui.set_min_width(180.0);
    let mut action = None;
    let mut fav = file.favorite;
    if ui.checkbox(&mut fav, theme::icon_text(icons::STAR, "favorite", 14.0, egui::FontFamily::Proportional, Color32::PLACEHOLDER)).on_hover_text("F").changed() {
        action = Some(FileAction::Favorite);
    }
    ui.separator();
    for (k, t) in defs.iter().enumerate() {
        let mut on = file.tags.contains(&t.name);
        let color = theme.color_of(k, Some(t)).gamma_multiply(1.3).to_opaque();
        let label = ui.checkbox(&mut on, RichText::new(&t.name).color(color));
        let label = if k < 9 { label.on_hover_text(format!("key {}", k + 1)) } else { label };
        if label.changed() {
            action = Some(FileAction::Tag(t.name.clone()));
        }
    }
    ui.separator();
    if ui.button(theme::icon_text(icons::FOLDER_OPEN, "show in folder", 14.0, egui::FontFamily::Proportional, Color32::PLACEHOLDER)).clicked() {
        action = Some(FileAction::Reveal);
        ui.close();
    }
    if ui.button(theme::icon_text(icons::COPY, "copy path", 14.0, egui::FontFamily::Proportional, Color32::PLACEHOLDER)).clicked() {
        ui.ctx().copy_text(file.path.display().to_string());
        action = Some(FileAction::CopyPath);
        ui.close();
    }
    action
}

/// Opens the file's folder in the desktop's file manager, selecting the file where that is
/// supported.
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
fn preset_summary(p: &hark_core::ExportPreset) -> [String; 2] {
    use hark_core::config::Normalize;
    let rate = p.sample_rate.map(|r| format!("{:.1} kHz", r as f64 / 1000.0)).unwrap_or_else(|| "source rate".into());
    let channels = if p.mono.is_some() { "mono" } else { "source ch" };
    let dither = if p.dither && p.bits < 32 { " dither" } else { "" };
    let format = format!("{} {} bit{dither} · {rate} · {channels}", p.format.extension().to_uppercase(), p.bits);
    let mut steps: Vec<String> = Vec::new();
    if let Some(t) = p.trim_silence {
        steps.push(format!("trim below {} dBFS", t.threshold_dbfs));
    }
    steps.push(match p.normalize {
        Normalize::None => "no normalisation".into(),
        Normalize::Peak { target_dbfs } => format!("peak to {target_dbfs} dBFS"),
        Normalize::Lufs { target_lufs, true_peak_ceiling_dbfs } => format!("{target_lufs} LUFS, ceiling {true_peak_ceiling_dbfs} dBTP"),
    });
    [format, steps.join(" · ")]
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let zoom = ctx.zoom_factor();
        if (zoom - self.config.ui_zoom).abs() > 1e-3 {
            self.config.ui_zoom = zoom;
            let _ = self.config.save();
        }
        self.handle_done();
        if self.list_dirty {
            self.refresh_list();
        }
        self.handle_keys(&ctx);

        let pad = theme::PAD;
        let bar = egui::Frame::new().fill(self.theme.bg_deep).inner_margin(Margin::symmetric(pad, 8));
        egui::Panel::top("top").frame(bar).show(ui, |ui| {
            self.ui_top(ui);
        });
        egui::Panel::bottom("status").frame(egui::Frame::new().fill(self.theme.bg_deep).inner_margin(Margin::symmetric(pad, 4))).show(ui, |ui| {
            self.ui_status(ui);
        });
        egui::Panel::bottom("bottom").resizable(true).default_size(350.0).frame(egui::Frame::new().fill(self.theme.bg).inner_margin(Margin { left: pad, right: pad, top: 8, bottom: 8 })).show(ui, |ui| {
            self.ui_bottom(ui);
        });
        egui::Panel::left("tree").resizable(true).default_size(250.0).frame(egui::Frame::new().fill(self.theme.bg_deep).inner_margin(Margin { left: 4, right: 4, top: 8, bottom: 8 })).show(ui, |ui| {
            self.ui_tree(ui);
        });
        egui::CentralPanel::default().frame(egui::Frame::new().fill(self.theme.bg).inner_margin(Margin { left: pad, right: pad, top: 8, bottom: 0 })).show(ui, |ui| {
            self.ui_list(ui);
        });

        self.ui_settings(&ctx);

        if self.is_playing() || !self.pending.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(16));
        }
    }
}

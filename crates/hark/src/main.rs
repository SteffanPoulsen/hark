#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod waveform;
mod worker;

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use egui::{Color32, Key, RichText, Sense};
use egui_extras::{Column, TableBuilder};
use hark_core::audition::{Clip, Player};
use hark_core::export::{ExportRequest, Selection};
use hark_core::library::Query;
use hark_core::{Config, Db, FileRecord};

use waveform::{WaveEvent, WaveState};
use worker::{Done, Job, Loaded, Worker};

const CACHE_FRAMES: usize = 48_000 * 60 * 20;
const PRELOAD: usize = 3;

fn main() -> eframe::Result {
    let mut open: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--open" => open = args.next().map(PathBuf::from),
            "-h" | "--help" => {
                println!("hark [--open <audio file or folder>]");
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
            Ok(Box::new(app))
        }),
    )
}

struct App {
    config: Config,
    db: Db,
    player: Option<Player>,
    audio_error: Option<String>,
    worker: Worker,

    expanded: HashSet<PathBuf>,
    dir_children: HashMap<PathBuf, Vec<PathBuf>>,
    selected_dir: Option<PathBuf>,

    search: String,
    filter_tag: Option<String>,
    fav_only: bool,
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
    gain_db: f32,
    preset: String,
    overwrite: bool,
    status: String,
    status_is_error: bool,
    scanning: bool,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let config = Config::load().unwrap_or_else(|e| {
            eprintln!("config: {e:#}");
            Config::default()
        });
        let db = Db::open().expect("library database");
        let (player, audio_error) = match Player::open(config.audition_sample_rate) {
            Ok(p) => (Some(p), None),
            Err(e) => (None, Some(format!("{e:#}"))),
        };
        cc.egui_ctx.set_zoom_factor(config.ui_zoom.clamp(0.5, 3.0));
        let worker = Worker::start(cc.egui_ctx.clone(), 3);
        if !config.roots.is_empty() {
            worker.send(Job::Scan(config.roots.clone()));
        }
        let preset = config.default_preset.clone();
        let mut app = App {
            config,
            db,
            player,
            audio_error,
            worker,
            expanded: HashSet::new(),
            dir_children: HashMap::new(),
            selected_dir: None,
            search: String::new(),
            filter_tag: None,
            fav_only: false,
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
            gain_db: 0.0,
            preset,
            overwrite: false,
            status: String::new(),
            status_is_error: false,
            scanning: true,
        };
        app.scanning = !app.config.roots.is_empty();
        for r in app.config.roots.clone() {
            app.expanded.insert(r.path.clone());
        }
        app
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
        self.selected_dir = Some(dir);
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
        let result = if self.search.trim().is_empty() {
            match &self.selected_dir {
                Some(d) => self.db.list_dir(d),
                None => Ok(Vec::new()),
            }
        } else {
            self.db.search(&Query { text: self.search.clone(), tag: self.filter_tag.clone(), favorites_only: self.fav_only, under: None, limit: 2000 })
        };
        let mut files = match result {
            Ok(f) => f,
            Err(e) => {
                self.set_status(format!("library: {e:#}"), true);
                Vec::new()
            }
        };
        if self.search.trim().is_empty() {
            files.retain(|f| (!self.fav_only || f.favorite) && self.filter_tag.as_ref().map(|t| f.tags.contains(t)).unwrap_or(true));
        }
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

    fn toggle_favorite(&mut self) {
        let Some(i) = self.selected else { return };
        let path = self.files[i].path.clone();
        let on = !self.files[i].favorite;
        match self.db.ensure_hash(&path).and_then(|h| self.db.set_favorite(&h, on)) {
            Ok(()) => self.refresh_record(i),
            Err(e) => self.set_status(format!("favorite: {e:#}"), true),
        }
    }

    fn toggle_tag(&mut self, tag: &str) {
        let Some(i) = self.selected else { return };
        let path = self.files[i].path.clone();
        let on = !self.files[i].tags.iter().any(|t| t == tag);
        match self.db.ensure_hash(&path).and_then(|h| self.db.set_tag(&h, tag, on)) {
            Ok(()) => self.refresh_record(i),
            Err(e) => self.set_status(format!("tag: {e:#}"), true),
        }
    }

    fn export(&mut self) {
        let Some(i) = self.selected else { return };
        let f = &self.files[i];
        let mut req = match ExportRequest::from_config(&self.config, &f.path, Some(&self.preset), f.tags.clone(), f.favorite) {
            Ok(r) => r,
            Err(e) => {
                self.set_status(format!("export: {e:#}"), true);
                return;
            }
        };
        req.selection = self.wave.selection.map(|(a, b)| Selection { start_secs: a, end_secs: b });
        req.fade_in_secs = self.wave.fade_in;
        req.fade_out_secs = self.wave.fade_out;
        req.preset.fade_shape = self.wave.fade_shape;
        req.overwrite = self.overwrite;
        self.set_status(format!("exporting {} → {}", f.name, req.output_path().display()), false);
        self.worker.send(Job::Export(req));
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
                        let mut msg = format!("exported {} · {:+.2} dB", r.output.display(), r.origin.measured.applied_gain_db);
                        for w in &r.warnings {
                            msg.push_str(" · ");
                            msg.push_str(w);
                        }
                        self.set_status(msg, false);
                    }
                    Err(e) => self.set_status(format!("export failed: {e}"), true),
                },
                Done::Scanned(reports) => {
                    self.scanning = false;
                    let total: usize = reports.iter().map(|(_, r)| r.seen).sum();
                    let errors: usize = reports.iter().map(|(_, r)| r.errors.len()).sum();
                    self.set_status(format!("scanned {total} files{}", if errors > 0 { format!(", {errors} unreadable") } else { String::new() }), errors > 0);
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
        if pressed(Key::ArrowDown) && n > 0 {
            let i = self.selected.map(|i| (i + 1).min(n - 1)).unwrap_or(0);
            self.select(i, self.autoplay);
        }
        if pressed(Key::ArrowUp) && n > 0 {
            let i = self.selected.map(|i| i.saturating_sub(1)).unwrap_or(0);
            self.select(i, self.autoplay);
        }
        if pressed(Key::PageDown) && n > 0 {
            let i = self.selected.map(|i| (i + 10).min(n - 1)).unwrap_or(0);
            self.select(i, self.autoplay);
        }
        if pressed(Key::PageUp) && n > 0 {
            let i = self.selected.map(|i| i.saturating_sub(10)).unwrap_or(0);
            self.select(i, self.autoplay);
        }
        if pressed(Key::Home) && n > 0 {
            self.select(0, self.autoplay);
        }
        if pressed(Key::End) && n > 0 {
            self.select(n - 1, self.autoplay);
        }
        if pressed(Key::Space) {
            if self.player.as_ref().map(|p| p.is_playing()).unwrap_or(false) && !shift {
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

    fn ui_top(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("hark").strong().size(18.0));
            ui.separator();
            let search = ui.add(egui::TextEdit::singleline(&mut self.search).hint_text("search the whole library").desired_width(260.0));
            if search.changed() {
                self.list_dirty = true;
            }
            if ui.input(|i| i.key_pressed(Key::Escape)) && search.has_focus() {
                self.search.clear();
                self.list_dirty = true;
                search.surrender_focus();
            }
            egui::ComboBox::from_id_salt("tag-filter")
                .selected_text(self.filter_tag.clone().unwrap_or_else(|| "any tag".into()))
                .show_ui(ui, |ui| {
                    if ui.selectable_value(&mut self.filter_tag, None, "any tag").clicked() {
                        self.list_dirty = true;
                    }
                    for t in self.config.tags.clone() {
                        if ui.selectable_value(&mut self.filter_tag, Some(t.name.clone()), &t.name).clicked() {
                            self.list_dirty = true;
                        }
                    }
                });
            if ui.selectable_label(self.fav_only, "★ favorites").clicked() {
                self.fav_only = !self.fav_only;
                self.list_dirty = true;
            }
            ui.separator();
            if ui.button("add library folder").clicked() {
                if let Some(path) = rfd::FileDialog::new().pick_folder() {
                    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "root".into());
                    if !self.config.roots.iter().any(|r| r.path == path) {
                        self.config.roots.push(hark_core::config::Root { name, path: path.clone() });
                        let _ = self.config.save();
                        self.expanded.insert(path.clone());
                        self.scanning = true;
                        self.worker.send(Job::Scan(self.config.roots.clone()));
                    }
                }
            }
            if ui.add_enabled(!self.scanning && !self.config.roots.is_empty(), egui::Button::new("rescan")).clicked() {
                self.scanning = true;
                self.dir_children.clear();
                self.worker.send(Job::Scan(self.config.roots.clone()));
            }
            if self.scanning {
                ui.spinner();
                ui.weak("scanning");
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                match (&self.player, &self.audio_error) {
                    (Some(p), _) => ui.weak(format!("{} · {} Hz", p.device_name, p.sample_rate)),
                    (None, Some(e)) => ui.colored_label(Color32::from_rgb(255, 120, 120), format!("no audio: {e}")),
                    _ => ui.weak("no audio"),
                };
            });
        });
    }

    fn ui_tree(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            if self.config.roots.is_empty() {
                ui.weak("No library folders yet.");
                ui.weak("Add one with the button above.");
                return;
            }
            let roots = self.config.roots.clone();
            for root in roots {
                self.ui_tree_node(ui, &root.path, &root.name, 0);
            }
        });
    }

    fn ui_tree_node(&mut self, ui: &mut egui::Ui, dir: &Path, label: &str, depth: usize) {
        let children = self.children_of(dir);
        let expanded = self.expanded.contains(dir);
        let selected = self.selected_dir.as_deref() == Some(dir);
        ui.horizontal(|ui| {
            ui.add_space(depth as f32 * 14.0);
            let arrow = if children.is_empty() { "  " } else if expanded { "▾" } else { "▸" };
            if ui.add(egui::Label::new(RichText::new(arrow).monospace()).sense(Sense::click())).clicked() && !children.is_empty() {
                if expanded {
                    self.expanded.remove(dir);
                } else {
                    self.expanded.insert(dir.to_path_buf());
                }
            }
            let text = if selected { RichText::new(label).strong().color(Color32::from_rgb(255, 210, 120)) } else { RichText::new(label) };
            let resp = ui.selectable_label(selected, text);
            if resp.clicked() {
                self.selected_dir = Some(dir.to_path_buf());
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
        });
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
            format!("{n} results")
        } else {
            match &self.selected_dir {
                Some(d) => format!("{} · {n} files", d.display()),
                None => "pick a folder".to_string(),
            }
        };
        ui.horizontal(|ui| {
            ui.weak(heading);
        });
        let row_h = 22.0;
        let mut clicked: Option<usize> = None;
        let mut double: Option<usize> = None;
        let mut fav_toggle: Option<usize> = None;
        let mut builder = TableBuilder::new(ui)
            .striped(true)
            .sense(Sense::click())
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::exact(22.0))
            .column(Column::remainder().at_least(220.0).clip(true))
            .column(Column::initial(160.0).at_least(60.0).clip(true))
            .column(Column::exact(72.0))
            .column(Column::exact(28.0))
            .column(Column::exact(52.0))
            .column(Column::exact(40.0))
            .column(Column::exact(70.0))
            .column(Column::exact(60.0));
        if self.scroll_to_selected {
            if let Some(i) = self.selected {
                builder = builder.scroll_to_row(i, None);
            }
            self.scroll_to_selected = false;
        }
        let files = &self.files;
        let selected = self.selected;
        builder
            .header(22.0, |mut h| {
                for title in ["★", "name", "tags", "length", "ch", "rate", "bits", "codec", "LUFS"] {
                    h.col(|ui| {
                        ui.strong(title);
                    });
                }
            })
            .body(|body| {
                body.rows(row_h, n, |mut row| {
                    let i = row.index();
                    let f = &files[i];
                    row.set_selected(selected == Some(i));
                    let (_, star) = row.col(|ui| {
                        ui.label(if f.favorite { RichText::new("★").color(Color32::from_rgb(255, 200, 80)) } else { RichText::new("☆").weak() });
                    });
                    if star.clicked() {
                        fav_toggle = Some(i);
                    }
                    row.col(|ui| {
                        let text = if f.error.is_some() { RichText::new(&f.name).color(Color32::from_rgb(255, 120, 120)) } else { RichText::new(&f.name) };
                        ui.label(text);
                    });
                    row.col(|ui| {
                        ui.label(RichText::new(f.tags.join(" ")).weak());
                    });
                    let info = f.info.as_ref();
                    row.col(|ui| {
                        ui.label(info.map(|x| waveform::fmt_time(x.duration_secs)).unwrap_or_default());
                    });
                    row.col(|ui| {
                        ui.label(info.map(|x| x.channels.to_string()).unwrap_or_default());
                    });
                    row.col(|ui| {
                        ui.label(info.map(|x| format!("{:.1}k", x.sample_rate as f64 / 1000.0)).unwrap_or_default());
                    });
                    row.col(|ui| {
                        ui.label(info.and_then(|x| x.bits_per_sample).map(|b| b.to_string()).unwrap_or_default());
                    });
                    row.col(|ui| {
                        ui.label(RichText::new(info.map(|x| x.codec.clone()).unwrap_or_default()).weak());
                    });
                    row.col(|ui| {
                        ui.label(f.lufs.map(|l| format!("{l:.1}")).unwrap_or_default());
                    });
                    let resp = row.response();
                    if resp.double_clicked() {
                        double = Some(i);
                    } else if resp.clicked() {
                        clicked = Some(i);
                    }
                });
            });
        if let Some(i) = fav_toggle {
            self.selected = Some(i);
            self.toggle_favorite();
        }
        if let Some(i) = double {
            self.select(i, true);
        } else if let Some(i) = clicked {
            self.select(i, self.autoplay);
        }
    }

    fn ui_bottom(&mut self, ui: &mut egui::Ui) {
        let playing = self.player.as_ref().map(|p| p.is_playing()).unwrap_or(false);
        let playhead = match (&self.player, &self.current) {
            (Some(p), Some(c)) if playing => Some(p.position() as f64 / c.device.sample_rate as f64),
            _ => None,
        };
        let duration = self.current.as_ref().map(|c| c.duration_secs).unwrap_or(0.0);
        let peaks = self.current.as_ref().map(|c| c.peaks.clone());
        let event = waveform::show(ui, &mut self.wave, peaks.as_deref(), duration, playhead, 150.0);
        if let WaveEvent::Seek(t) = event {
            self.play(Some(t));
        }

        ui.horizontal(|ui| {
            let play_label = if playing { "■ stop" } else { "▶ play" };
            if ui.button(play_label).clicked() {
                if playing {
                    self.stop();
                } else {
                    self.play(None);
                }
            }
            ui.toggle_value(&mut self.loop_on, "loop");
            ui.toggle_value(&mut self.autoplay, "autoplay");
            ui.separator();
            ui.label("gain");
            if ui.add(egui::Slider::new(&mut self.gain_db, -36.0..=12.0).suffix(" dB").show_value(true)).changed() {
                if let Some(p) = &self.player {
                    p.set_gain(hark_core::pcm::db_to_gain(self.gain_db as f64));
                }
            }
            ui.separator();
            if let Some(c) = &self.current {
                let name = c.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                ui.label(RichText::new(name).strong());
                ui.weak(format!("{} Hz · {} ch · {}", c.source_rate, c.channels, waveform::fmt_time(c.duration_secs)));
                if let Some(l) = c.loudness {
                    ui.weak(format!("{:.1} LUFS{} · {:.1} dBTP", l.integrated_lufs, if l.momentary { " (M)" } else { "" }, l.true_peak_dbfs));
                }
                if let Some(t) = playhead {
                    ui.monospace(waveform::fmt_time(t));
                }
            } else if let Some(i) = self.selected {
                if self.pending.contains(&self.files[i].path) {
                    ui.spinner();
                    ui.weak("loading");
                }
            }
        });

        ui.horizontal(|ui| {
            ui.label("selection");
            let mut sel = self.wave.selection.unwrap_or((0.0, 0.0));
            let had = self.wave.selection.is_some();
            let a = ui.add(egui::DragValue::new(&mut sel.0).speed(0.01).range(0.0..=duration).suffix(" s").fixed_decimals(3));
            let b = ui.add(egui::DragValue::new(&mut sel.1).speed(0.01).range(0.0..=duration).suffix(" s").fixed_decimals(3));
            if (a.changed() || b.changed()) && (had || sel.1 > sel.0) {
                self.wave.selection = if sel.1 > sel.0 { Some(sel) } else { None };
            }
            if ui.small_button("clear").clicked() {
                self.wave.selection = None;
            }
            if ui.small_button("zoom").clicked() {
                self.wave.zoom_to_selection();
            }
            ui.separator();
            ui.label("fade in");
            ui.add(egui::DragValue::new(&mut self.wave.fade_in).speed(0.01).range(0.0..=duration).suffix(" s").fixed_decimals(3));
            ui.label("out");
            ui.add(egui::DragValue::new(&mut self.wave.fade_out).speed(0.01).range(0.0..=duration).suffix(" s").fixed_decimals(3));
            egui::ComboBox::from_id_salt("fade-shape")
                .selected_text(format!("{:?}", self.wave.fade_shape))
                .show_ui(ui, |ui| {
                    for s in [hark_core::pcm::FadeShape::EqualPower, hark_core::pcm::FadeShape::Linear, hark_core::pcm::FadeShape::Logarithmic] {
                        ui.selectable_value(&mut self.wave.fade_shape, s, format!("{s:?}"));
                    }
                });
            self.wave.clamp_fades();
            ui.separator();
            if let Some(i) = self.selected {
                let tags = self.files[i].tags.clone();
                for (k, t) in self.config.tags.clone().iter().enumerate() {
                    let on = tags.contains(&t.name);
                    let label = if k < 9 { format!("{} {}", k + 1, t.name) } else { t.name.clone() };
                    if ui.selectable_label(on, label).clicked() {
                        self.toggle_tag(&t.name);
                    }
                }
            }
        });

        ui.horizontal(|ui| {
            ui.label("preset");
            egui::ComboBox::from_id_salt("preset").selected_text(&self.preset).show_ui(ui, |ui| {
                for name in self.config.presets.keys().cloned().collect::<Vec<_>>() {
                    ui.selectable_value(&mut self.preset, name.clone(), name);
                }
            });
            if let Some(p) = self.config.preset(&self.preset) {
                ui.weak(format!(
                    "{} {}bit · {} · {} · {:?}",
                    p.format.extension(),
                    p.bits,
                    p.sample_rate.map(|r| format!("{r} Hz")).unwrap_or_else(|| "keep rate".into()),
                    if p.mono.is_some() { "mono" } else { "keep channels" },
                    p.normalize
                ));
            }
            ui.separator();
            ui.label("output");
            let out = self.config.output_root.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "not set".into());
            if ui.button(out).clicked() {
                if let Some(path) = rfd::FileDialog::new().pick_folder() {
                    self.config.output_root = Some(path);
                    let _ = self.config.save();
                }
            }
            ui.checkbox(&mut self.overwrite, "overwrite");
            let can = self.selected.is_some() && self.config.output_root.is_some();
            if ui.add_enabled(can, egui::Button::new("export  (E)")).clicked() {
                self.export();
            }
            if let (Some(i), Some(_)) = (self.selected, &self.config.output_root) {
                let f = &self.files[i];
                if let Ok(req) = ExportRequest::from_config(&self.config, &f.path, Some(&self.preset), f.tags.clone(), f.favorite) {
                    ui.weak(format!("→ {}", req.output_path().strip_prefix(&req.output_root).unwrap_or(&req.output_path()).display()));
                }
            }
        });

        if !self.status.is_empty() {
            let color = if self.status_is_error { Color32::from_rgb(255, 130, 130) } else { Color32::from_rgb(140, 220, 140) };
            ui.colored_label(color, &self.status);
        }
    }
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

        egui::Panel::top("top").show(ui, |ui| {
            ui.add_space(4.0);
            self.ui_top(ui);
            ui.add_space(4.0);
        });
        egui::Panel::bottom("bottom").resizable(true).default_size(290.0).show(ui, |ui| {
            ui.add_space(4.0);
            self.ui_bottom(ui);
            ui.add_space(4.0);
        });
        egui::Panel::left("tree").resizable(true).default_size(260.0).show(ui, |ui| {
            self.ui_tree(ui);
        });
        egui::CentralPanel::default().show(ui, |ui| {
            self.ui_list(ui);
        });

        if self.player.as_ref().map(|p| p.is_playing()).unwrap_or(false) || !self.pending.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(16));
        }
    }
}

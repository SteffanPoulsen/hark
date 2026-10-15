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
use hark_core::library::Query;
use hark_core::{Config, Db, FileRecord};

use theme::Theme;
use waveform::{WaveEvent, WaveState};
use worker::{Done, Job, Loaded, Worker};

const CACHE_FRAMES: usize = 48_000 * 60 * 20;
const PRELOAD: usize = 3;
const EXPORT_CARD_W: f32 = 330.0;

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
    theme: Theme,
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
    last_export: Option<Result<String, String>>,
    scanning: bool,
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
        let db = Db::open().expect("library database");
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
        let expanded = config.roots.iter().map(|r| r.path.clone()).collect();
        App {
            config,
            theme,
            db,
            player,
            audio_error,
            worker,
            expanded,
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
            last_export: None,
            scanning,
            file_count: 0,
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
            ui.label(theme::semibold("hark").size(18.0).color(self.theme.accent));
            ui.add_space(8.0);
            let search = ui.add(egui::TextEdit::singleline(&mut self.search).hint_text(theme::icon_text(icons::MAGNIFYING_GLASS, "search the library", 14.0, egui::FontFamily::Proportional, self.theme.muted)).desired_width(280.0));
            if search.changed() {
                self.list_dirty = true;
            }
            if ui.input(|i| i.key_pressed(Key::Escape)) && search.has_focus() {
                self.search.clear();
                self.list_dirty = true;
                search.surrender_focus();
            }
            egui::ComboBox::from_id_salt("tag-filter")
                .selected_text(theme::icon_text(icons::TAG, self.filter_tag.as_deref().unwrap_or("any tag"), 14.0, egui::FontFamily::Proportional, Color32::PLACEHOLDER))
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
            if Self::icon_button(ui, icons::STAR, "favorites", self.fav_only).clicked() {
                self.fav_only = !self.fav_only;
                self.list_dirty = true;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if Self::icon_button(ui, icons::FOLDER_PLUS, "add folder", false).clicked() {
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
                if ui.add_enabled(!self.scanning && !self.config.roots.is_empty(), egui::Button::new(theme::icon_text(icons::ARROWS_CLOCKWISE, "rescan", 14.0, egui::FontFamily::Proportional, Color32::PLACEHOLDER))).clicked() {
                    self.scanning = true;
                    self.dir_children.clear();
                    self.worker.send(Job::Scan(self.config.roots.clone()));
                }
            });
        });
    }

    fn ui_tree(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.add_space(4.0);
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
        let row_h = 24.0;
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
        if !children.is_empty() {
            let caret = if expanded { icons::CARET_DOWN } else { icons::CARET_RIGHT };
            p.text(egui::pos2(x + 2.0, rect.center().y), egui::Align2::LEFT_CENTER, caret, icon_font.clone(), self.theme.muted);
        }
        let folder = if expanded && !children.is_empty() { icons::FOLDER_OPEN } else { icons::FOLDER };
        p.text(egui::pos2(x + 20.0, rect.center().y), egui::Align2::LEFT_CENTER, folder, icon_font, if selected { self.theme.accent } else { self.theme.muted });
        p.text(egui::pos2(x + 40.0, rect.center().y), egui::Align2::LEFT_CENTER, label, font, text_color);

        if caret_resp.clicked() && !children.is_empty() {
            if expanded {
                self.expanded.remove(dir);
            } else {
                self.expanded.insert(dir.to_path_buf());
            }
        } else if resp.clicked() {
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
                Some(d) => {
                    let shown = self.config.root_of(d).and_then(|r| d.strip_prefix(&r.path).ok()).map(|rel| if rel.as_os_str().is_empty() { String::new() } else { format!("  /  {}", rel.display()) });
                    match (self.config.root_of(d), shown) {
                        (Some(r), Some(rel)) => format!("{}{rel}   ·   {n} files", r.name),
                        _ => format!("{}   ·   {n} files", d.display()),
                    }
                }
                None => "pick a folder".to_string(),
            }
        };
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.add_space(6.0);
            ui.label(RichText::new(heading).color(self.theme.muted));
        });
        ui.add_space(4.0);
        let row_h = 26.0;
        let mut clicked: Option<usize> = None;
        let mut double: Option<usize> = None;
        let mut fav_toggle: Option<usize> = None;
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
        let tag_index: HashMap<String, usize> = self.config.tags.iter().enumerate().map(|(i, t)| (t.name.clone(), i)).collect();
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
                        let hot = ui.ui_contains_pointer();
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
                            let k = tag_index.get(t).copied().unwrap_or(theme.tag_colors.len() - 1);
                            chip(ui, t, theme.tag_color(k));
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
                    }
                });
            });
        if let Some(i) = fav_toggle {
            self.toggle_favorite_of(i);
        }
        if let Some(i) = double {
            self.select(i, true);
        } else if let Some(i) = clicked {
            self.select(i, self.autoplay);
        }
    }

    fn ui_bottom(&mut self, ui: &mut egui::Ui) {
        egui::Panel::right("export").resizable(false).default_size(EXPORT_CARD_W).show(ui, |ui| {
            ui.add_space(6.0);
            self.ui_export_card(ui);
        });
        egui::CentralPanel::default().frame(egui::Frame::new().inner_margin(Margin { left: 0, right: 8, top: 6, bottom: 4 })).show(ui, |ui| {
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

        // File header.
        ui.horizontal(|ui| {
            if let Some(c) = self.current.clone() {
                let name = c.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                ui.label(theme::semibold(name).size(15.0));
                let mut meta = format!("{} Hz  ·  {} ch  ·  {}", c.source_rate, c.channels, waveform::fmt_time(c.duration_secs));
                if let Some(l) = c.loudness {
                    meta.push_str(&format!("  ·  {:.1} LUFS{}  ·  {:.1} dBTP", l.integrated_lufs, if l.momentary { " (M)" } else { "" }, l.true_peak_dbfs));
                }
                ui.label(RichText::new(meta).color(self.theme.muted));
                if let Some(i) = self.selected {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        let defs = self.config.tags.clone();
                        let tags = self.files[i].tags.clone();
                        let mut toggled = None;
                        for (k, t) in defs.iter().enumerate().rev() {
                            let on = tags.contains(&t.name);
                            if self.chip(ui, &t.name, self.theme.tag_color(k), on, true).on_hover_text(format!("key {}", k + 1)).clicked() {
                                toggled = Some(t.name.clone());
                            }
                        }
                        if let Some(t) = toggled {
                            self.toggle_tag(&t);
                        }
                    });
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

        // Waveform.
        let transport_h = 34.0;
        let wave_h = (ui.available_height() - transport_h - 8.0).max(80.0);
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

        // Transport.
        ui.horizontal(|ui| {
            let play = if playing { icons::STOP } else { icons::PLAY };
            if ui.add(egui::Button::new(theme::icon(play).size(16.0)).min_size(egui::vec2(34.0, 26.0))).clicked() {
                if playing {
                    self.stop();
                } else {
                    self.play(None);
                }
            }
            if Self::icon_button(ui, icons::REPEAT, "", self.loop_on).on_hover_text("loop (L)").clicked() {
                self.loop_on = !self.loop_on;
            }
            if ui.add(egui::Button::new("auto").selected(self.autoplay)).on_hover_text("autoplay on selection (A)").clicked() {
                self.autoplay = !self.autoplay;
            }
            let t = playhead.unwrap_or(0.0);
            ui.label(RichText::new(format!("{}", waveform::fmt_time(t))).monospace().color(if playing { self.theme.text } else { self.theme.muted }));
            ui.scope(|ui| { ui.spacing_mut().slider_width = 80.0; ui.add(egui::Slider::new(&mut self.gain_db, -36.0..=12.0).suffix(" dB").show_value(true).trailing_fill(false)).on_hover_text("audition gain"); });
            if ui.input(|i| i.pointer.any_released()) {
                if let Some(p) = &self.player {
                    p.set_gain(hark_core::pcm::db_to_gain(self.gain_db as f64));
                }
            }
            ui.separator();
            ui.label(RichText::new("sel").color(self.theme.muted));
            let mut sel = self.wave.selection.unwrap_or((0.0, 0.0));
            let had = self.wave.selection.is_some();
            let a = ui.add(egui::DragValue::new(&mut sel.0).speed(0.01).range(0.0..=duration).fixed_decimals(3));
            let b = ui.add(egui::DragValue::new(&mut sel.1).speed(0.01).range(0.0..=duration).fixed_decimals(3));
            if (a.changed() || b.changed()) && (had || sel.1 > sel.0) {
                self.wave.selection = if sel.1 > sel.0 { Some(sel) } else { None };
            }
            if ui.add(egui::Button::new(theme::icon(icons::X)).small()).on_hover_text("clear selection (Del)").clicked() {
                self.wave.selection = None;
            }
            if ui.add(egui::Button::new(theme::icon(icons::MAGNIFYING_GLASS_PLUS)).small()).on_hover_text("zoom to selection (Z)").clicked() {
                self.wave.zoom_to_selection();
            }
            ui.separator();
            ui.label(RichText::new("fade").color(self.theme.muted));
            ui.add(egui::DragValue::new(&mut self.wave.fade_in).speed(0.01).range(0.0..=duration).fixed_decimals(3));
            ui.add(egui::DragValue::new(&mut self.wave.fade_out).speed(0.01).range(0.0..=duration).fixed_decimals(3));
            egui::ComboBox::from_id_salt("fade-shape")
                .width(60.0)
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

    fn ui_export_card(&mut self, ui: &mut egui::Ui) {
        let theme = self.theme.clone();
        egui::Frame::new().fill(theme.surface).corner_radius(CornerRadius::same(8)).inner_margin(Margin::same(12)).show(ui, |ui| {
            ui.set_min_height(ui.available_height());
            ui.label(theme::icon_text(icons::EXPORT, "Export", 15.0, egui::FontFamily::Name(theme::SEMIBOLD.into()), Color32::PLACEHOLDER));
            ui.add_space(6.0);
            egui::ComboBox::from_id_salt("preset").width(ui.available_width()).selected_text(&self.preset).show_ui(ui, |ui| {
                for name in self.config.presets.keys().cloned().collect::<Vec<_>>() {
                    ui.selectable_value(&mut self.preset, name.clone(), name);
                }
            });
            if let Some(p) = self.config.preset(&self.preset).cloned() {
                let lines = [
                    format!("{}  {} bit{}", p.format.extension().to_uppercase(), p.bits, if p.dither && p.bits < 32 { ", dithered" } else { "" }),
                    p.sample_rate.map(|r| format!("{} Hz", r)).unwrap_or_else(|| "source rate".into()),
                    match p.mono {
                        Some(_) => "mono".into(),
                        None => "source channels".into(),
                    },
                    match p.normalize {
                        hark_core::config::Normalize::None => "no normalisation".into(),
                        hark_core::config::Normalize::Peak { target_dbfs } => format!("peak to {target_dbfs} dBFS"),
                        hark_core::config::Normalize::Lufs { target_lufs, true_peak_ceiling_dbfs } => format!("{target_lufs} LUFS, ceiling {true_peak_ceiling_dbfs} dBTP"),
                    },
                    p.trim_silence.map(|t| format!("trim below {} dBFS, pad {} ms", t.threshold_dbfs, t.pad_ms)).unwrap_or_else(|| "no trim".into()),
                ];
                ui.add_space(2.0);
                for l in lines {
                    ui.label(RichText::new(l).color(theme.muted).size(12.5));
                }
            }
            ui.add_space(10.0);
            ui.label(RichText::new("output folder").color(theme.muted).size(12.0));
            let out = self.config.output_root.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "choose…".into());
            if ui.add(egui::Button::new(theme::icon_text(icons::FOLDER, &out, 12.5, egui::FontFamily::Proportional, Color32::PLACEHOLDER)).min_size(egui::vec2(ui.available_width(), 0.0)).wrap()).clicked() {
                if let Some(path) = rfd::FileDialog::new().pick_folder() {
                    self.config.output_root = Some(path);
                    let _ = self.config.save();
                }
            }
            ui.add_space(6.0);
            match self.export_request() {
                Some(Ok(req)) => {
                    let rel = req.output_path();
                    let rel = rel.strip_prefix(&req.output_root).map(|p| p.to_path_buf()).unwrap_or(rel);
                    let detail = match req.selection {
                        Some(s) => format!("{} – {}", waveform::fmt_time(s.start_secs), waveform::fmt_time(s.end_secs)),
                        None => "whole file".into(),
                    };
                    ui.label(RichText::new(format!("→ {}", rel.display())).size(12.5));
                    ui.label(RichText::new(detail).color(theme.muted).size(12.0));
                }
                Some(Err(e)) => {
                    ui.label(RichText::new(e).color(theme.warn).size(12.0));
                }
                None => {
                    ui.label(RichText::new("select a file").color(theme.muted).size(12.0));
                }
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.checkbox(&mut self.overwrite, "overwrite");
            });
            ui.add_space(4.0);
            let can = self.selected.is_some() && self.config.output_root.is_some();
            let button = egui::Button::new(theme::icon_text(icons::EXPORT, "Export", 14.0, egui::FontFamily::Name(theme::SEMIBOLD.into()), if can { Color32::WHITE } else { theme.muted }))
                .fill(if can { theme.accent } else { theme.overlay })
                .min_size(egui::vec2(ui.available_width(), 32.0));
            if ui.add_enabled(can, button).on_hover_text("E").clicked() {
                self.export();
            }
            if let Some(result) = &self.last_export {
                ui.add_space(8.0);
                match result {
                    Ok(msg) => {
                        for (k, line) in msg.lines().enumerate() {
                            ui.label(RichText::new(line).color(if k == 0 { theme.ok } else { theme.warn }).size(12.0));
                        }
                    }
                    Err(e) => {
                        ui.label(RichText::new(e).color(theme.err).size(12.0));
                    }
                }
            }
        });
    }

    fn ui_status(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.add_space(6.0);
            let color = if self.status_is_error { self.theme.err } else { self.theme.muted };
            ui.label(RichText::new(&self.status).color(color).size(12.0));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(6.0);
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

        let bar = egui::Frame::new().fill(self.theme.bg_deep).inner_margin(Margin::symmetric(6, 6));
        egui::Panel::top("top").frame(bar).show(ui, |ui| {
            self.ui_top(ui);
        });
        egui::Panel::bottom("status").frame(egui::Frame::new().fill(self.theme.bg_deep).inner_margin(Margin::symmetric(4, 3))).show(ui, |ui| {
            self.ui_status(ui);
        });
        egui::Panel::bottom("bottom").resizable(true).default_size(350.0).frame(egui::Frame::new().fill(self.theme.bg).inner_margin(Margin::symmetric(8, 4))).show(ui, |ui| {
            self.ui_bottom(ui);
        });
        egui::Panel::left("tree").resizable(true).default_size(250.0).frame(egui::Frame::new().fill(self.theme.bg_deep).inner_margin(Margin::symmetric(4, 4))).show(ui, |ui| {
            self.ui_tree(ui);
        });
        egui::CentralPanel::default().frame(egui::Frame::new().fill(self.theme.bg).inner_margin(Margin::symmetric(8, 0))).show(ui, |ui| {
            self.ui_list(ui);
        });

        if self.is_playing() || !self.pending.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(16));
        }
    }
}

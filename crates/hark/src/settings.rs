//! The settings window: a page list on the left, and on the right cards of rows that pair a
//! title and a one-line explanation with the control that changes it.

use egui::{Color32, CornerRadius, Margin, RichText};
use egui_phosphor::regular as icons;
use hark_core::audition::Player;
use hark_core::keys::Action;

use crate::theme::{self, Theme};
use crate::preset_ui;
use crate::{App, TagAction};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Page {
    General,
    Tags,
    Presets,
    Keys,
}

impl Page {
    const ALL: [Page; 4] = [Page::General, Page::Tags, Page::Presets, Page::Keys];

    fn title(self) -> &'static str {
        match self {
            Page::General => "General",
            Page::Tags => "Tags",
            Page::Presets => "Presets",
            Page::Keys => "Keys",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Page::General => icons::SLIDERS_HORIZONTAL,
            Page::Tags => icons::TAG,
            Page::Presets => icons::SLIDERS,
            Page::Keys => icons::KEYBOARD,
        }
    }
}

const NAV_W: f32 = 180.0;
const CONTENT_W: f32 = 720.0;
const RATES: [u32; 4] = [44_100, 48_000, 96_000, 192_000];

/// A rounded card holding rows.
fn card<R>(ui: &mut egui::Ui, theme: &Theme, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::new().fill(theme.surface).corner_radius(CornerRadius::same(8)).inner_margin(Margin::symmetric(16, 4)).show(ui, add).inner
}

/// One setting: title and description on the left, the control on the right, a hairline
/// between rows.
fn row(ui: &mut egui::Ui, theme: &Theme, first: bool, title: &str, desc: &str, control: impl FnOnce(&mut egui::Ui)) {
    if !first {
        ui.separator();
    }
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        let text_w = ui.available_width() * 0.58;
        ui.vertical(|ui| {
            ui.set_max_width(text_w);
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.label(title);
            if !desc.is_empty() {
                ui.label(RichText::new(desc).color(theme.muted).size(12.5));
            }
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), control);
    });
    ui.add_space(8.0);
}

fn heading(ui: &mut egui::Ui, theme: &Theme, text: &str, desc: &str) {
    ui.label(theme::semibold(text).size(17.0));
    if !desc.is_empty() {
        ui.label(RichText::new(desc).color(theme.muted).size(12.5));
    }
    ui.add_space(10.0);
}

impl App {
    pub fn ui_settings(&mut self, ctx: &egui::Context) {
        if !self.settings_open {
            self.outputs = None;
            return;
        }
        // A real window of its own, so the compositor tiles it like any other.
        let builder = egui::ViewportBuilder::default().with_title("hark settings").with_app_id("hark").with_inner_size([960.0, 720.0]).with_min_inner_size([640.0, 420.0]);
        ctx.show_viewport_immediate(egui::ViewportId::from_hash_of("settings"), builder, |ctx, _| {
            if ctx.input(|i| i.viewport().close_requested()) {
                self.settings_open = false;
                self.tag_edit = None;
                self.preset_rename = None;
                self.key_capture = None;
            }
            let theme = self.theme.clone();
            egui::Panel::left("settings-nav").resizable(false).exact_size(NAV_W).frame(egui::Frame::new().fill(theme.bg_deep).inner_margin(Margin::same(12))).show(ctx, |ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.add_space(4.0);
                ui.label(RichText::new("Options").color(theme.muted).size(12.0));
                ui.add_space(4.0);
                for page in Page::ALL {
                    let on = self.settings_page == page;
                    let text = theme::icon_text(page.icon(), page.title(), 14.0, egui::FontFamily::Proportional, Color32::PLACEHOLDER);
                    let button = egui::Button::new(text).selected(on).min_size(egui::vec2(ui.available_width(), 28.0)).fill(if on { theme.select } else { Color32::TRANSPARENT }).frame(on);
                    if ui.add(button).clicked() {
                        self.settings_page = page;
                    }
                }
            });
            egui::CentralPanel::default().frame(egui::Frame::new().fill(theme.bg).inner_margin(Margin::symmetric(24, 20))).show(ctx, |ui| {
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.set_max_width(CONTENT_W);
                        ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| match self.settings_page {
                            Page::General => self.page_general(ui),
                            Page::Tags => self.page_tags(ui),
                            Page::Presets => self.page_presets(ui),
                            Page::Keys => self.page_keys(ui),
                        });
                    });
                });
            });
        });
    }

    fn page_general(&mut self, ui: &mut egui::Ui) {
        let theme = self.theme.clone();
        heading(ui, &theme, "General", "");
        let mut save = false;
        let mut reopen_audio = false;
        let mut retheme = false;
        card(ui, &theme, |ui| {
            row(ui, &theme, true, "Window scale", "Ctrl+= and Ctrl+- change it too.", |ui| {
                ui.allocate_ui_with_layout(egui::vec2(200.0, 28.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    // The new scale applies once the drag ends; applying it live would rescale
                    // the slider under the pointer.
                    let mut z = self.zoom_pending.unwrap_or(self.config.ui_zoom);
                    let zoom = ui.add(egui::Slider::new(&mut z, 0.5..=3.0).step_by(0.05).fixed_decimals(2));
                    if zoom.changed() {
                        self.zoom_pending = Some(z);
                    }
                    if zoom.drag_stopped() || zoom.clicked() || zoom.lost_focus() {
                        if let Some(z) = self.zoom_pending.take() {
                            self.config.ui_zoom = z;
                            ui.ctx().set_zoom_factor(z);
                            save = true;
                        }
                    }
                });
            });
            row(ui, &theme, false, "Layout", "The panel sizes and list columns you dragged. A double-click on a separator resets that one.", |ui| {
                if ui.add(egui::Button::new(theme::icon_text(icons::ARROW_COUNTER_CLOCKWISE, "Reset", 14.0, egui::FontFamily::Proportional, Color32::PLACEHOLDER)).min_size(egui::vec2(0.0, 28.0))).clicked() {
                    self.reset_layout = true;
                    ui.ctx().request_repaint_of(egui::ViewportId::ROOT);
                }
            });
            let device = match (&self.player, &self.audio_error) {
                (Some(p), _) => format!("{}  ·  {} Hz  ·  {} ch. System default follows the desktop's choice.", p.device_name, p.sample_rate, p.channels),
                (None, Some(e)) => format!("No audio: {e}"),
                _ => "No audio".into(),
            };
            let outputs = self.outputs.get_or_insert_with(hark_core::audition::outputs).clone();
            row(ui, &theme, false, "Audio output", &device, |ui| {
                let current = self.config.output_device.clone();
                let label = |id: Option<&str>| match id {
                    None => "System default".to_string(),
                    Some(id) => outputs.iter().find(|o| o.id == id).map(|o| o.name.clone()).unwrap_or_else(|| format!("{} (not found)", id.rsplit(':').next().unwrap_or(id))),
                };
                let r = egui::ComboBox::from_id_salt("output-device").width(220.0).truncate().selected_text(label(current.as_deref())).show_ui(ui, |ui| {
                    let mut pick = |ui: &mut egui::Ui, id: Option<&str>| {
                        if ui.selectable_label(current.as_deref() == id, label(id)).clicked() && current.as_deref() != id {
                            self.config.output_device = id.map(str::to_string);
                            save = true;
                            reopen_audio = true;
                        }
                    };
                    pick(ui, None);
                    for o in &outputs {
                        pick(ui, Some(&o.id));
                    }
                });
                r.response.on_hover_text(label(current.as_deref()));
            });
            row(ui, &theme, false, "Sample rate", "The rate the output is opened at; sources are resampled to it for playback.", |ui| {
                let current = self.config.output_sample_rate;
                let label = |r: Option<u32>| r.map(|r| format!("{:.1} kHz", r as f64 / 1000.0)).unwrap_or_else(|| "Device default".into());
                egui::ComboBox::from_id_salt("output-rate").width(150.0).selected_text(label(current)).show_ui(ui, |ui| {
                    let mut pick = |ui: &mut egui::Ui, r: Option<u32>| {
                        if ui.selectable_label(current == r, label(r)).clicked() && current != r {
                            self.config.output_sample_rate = r;
                            save = true;
                            reopen_audio = true;
                        }
                    };
                    pick(ui, None);
                    for r in RATES {
                        pick(ui, Some(r));
                    }
                });
            });
            row(ui, &theme, false, "Stream above", "Files whose decoded audio is larger than this stream from disk while they play instead of being decoded whole. 256 MB is about ten minutes of 48 kHz stereo.", |ui| {
                let r = ui.add(egui::DragValue::new(&mut self.config.stream_above_mb).speed(4.0).range(16.0..=16384.0).suffix(" MB").fixed_decimals(0));
                save |= r.drag_stopped() || r.lost_focus();
            });
            self.copy_rows(ui, &theme, &mut save);
            let palette = self.config.theme_file.as_ref().map(|p| p.display().to_string());
            row(ui, &theme, false, "Theme palette", "A palette file in the desktop theme format replaces the built-in colors.", |ui| {
                if palette.is_some() && theme::tool_button(ui, icons::X, "").on_hover_text("Use the built-in palette").clicked() {
                    self.config.theme_file = None;
                    save = true;
                    retheme = true;
                }
                let label = palette.as_deref().and_then(|p| p.rsplit('/').next().map(str::to_string)).unwrap_or_else(|| "Choose…".into());
                let b = ui.add(egui::Button::new(theme::icon_text(icons::FILE, &label, 14.0, egui::FontFamily::Proportional, Color32::PLACEHOLDER)).min_size(egui::vec2(0.0, 28.0)));
                let b = match &palette {
                    Some(p) => b.on_hover_text(p),
                    None => b,
                };
                if b.clicked() {
                    if let Some(path) = rfd::FileDialog::new().add_filter("palette", &["json"]).pick_file() {
                        self.config.theme_file = Some(path);
                        save = true;
                        retheme = true;
                    }
                }
            });
        });
        if save {
            let _ = self.config.save();
        }
        if reopen_audio {
            self.reopen_audio();
        }
        if retheme {
            self.theme = self.config.theme_file.as_ref().and_then(|p| Theme::from_file(p)).unwrap_or_default();
            self.theme.apply(ui.ctx());
        }
    }

    /// The copy folder: where it is and what it holds, with a way to move, open and
    /// clear it; and the open project's own folder, when there is a project.
    fn copy_rows(&mut self, ui: &mut egui::Ui, theme: &Theme, save: &mut bool) {
        let folder = self.config.copy_folder();
        let (summary, _) = hark_core::copies::list(&folder);
        let own = self.config.copy_folder.is_some();
        let desc = format!(
            "Where a dragged or copied selection is written: {}  ·  {} file{}, {:.1} MB. A DAW may reference a copy in place, so nothing is removed until you clear the folder.",
            crate::tilde(&folder),
            summary.files,
            if summary.files == 1 { "" } else { "s" },
            summary.bytes as f64 / 1e6
        );
        let project_folder = self.config.active.as_ref().and_then(|a| a.project.copy_folder.clone());
        let title = if project_folder.is_some() { "Default copy folder" } else { "Copy folder" };
        let mut clear = false;
        row(ui, theme, false, title, &desc, |ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            let b = egui::Button::new(theme::icon_text(icons::TRASH, "Clear", 14.0, egui::FontFamily::Proportional, theme.err)).min_size(egui::vec2(0.0, 28.0));
            if ui.add_enabled(summary.files > 0, b).on_hover_text("Remove every copy").clicked() {
                clear = true;
            }
            if theme::tool_button(ui, icons::FOLDER_OPEN, "").on_hover_text("Show the folder").clicked() {
                let _ = std::fs::create_dir_all(&folder);
                if let Err(e) = crate::reveal(&folder) {
                    self.set_status(format!("Show in folder: {e}"), true);
                }
            }
            if own && theme::tool_button(ui, icons::X, "").on_hover_text("Back to hark's own folder").clicked() {
                self.config.copy_folder = None;
                *save = true;
            }
            if theme::tool_button(ui, icons::FOLDER, "Choose…").clicked() {
                if let Some(path) = rfd::FileDialog::new().pick_folder() {
                    self.config.copy_folder = Some(path);
                    *save = true;
                }
            }
        });
        if clear {
            self.copies_clear = true;
        }
        if self.copies_clear {
            let modal = egui::Modal::new(egui::Id::new("copies-clear")).show(ui.ctx(), |ui| {
                ui.set_width(360.0);
                ui.label(theme::semibold("Clear the copy folder?"));
                ui.add_space(6.0);
                ui.label(RichText::new(format!("{} files in {} go. A DAW session that references one of them in place loses it.", summary.files, crate::tilde(&folder))).size(13.0));
                ui.add_space(12.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(RichText::new("Clear").color(theme.err)).clicked() {
                        match hark_core::copies::clear(&folder) {
                            Ok(n) => self.set_status(format!("Removed {n} copies"), false),
                            Err(e) => self.set_status(format!("Clear copies: {e:#}"), true),
                        }
                        self.copies_clear = false;
                    }
                    if ui.button("Keep").clicked() {
                        self.copies_clear = false;
                    }
                });
            });
            if modal.should_close() {
                self.copies_clear = false;
            }
        }
        if let Some(active) = &self.config.active {
            let name = hark_core::project::name_of_path(&active.path);
            let desc = match &project_folder {
                Some(p) => format!("{}'s copies go to {} instead of the default folder.", name, crate::tilde(p)),
                None => format!("{name} uses the default folder. A project at a DAW session can send its copies to the session's audio folder instead."),
            };
            let mut set: Option<Option<std::path::PathBuf>> = None;
            row(ui, theme, false, "Project copy folder", &desc, |ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                if project_folder.is_some() && theme::tool_button(ui, icons::X, "").on_hover_text("Use the default folder").clicked() {
                    set = Some(None);
                }
                if theme::tool_button(ui, icons::FOLDER, "Choose…").clicked() {
                    if let Some(path) = rfd::FileDialog::new().pick_folder() {
                        set = Some(Some(path));
                    }
                }
            });
            if let Some(value) = set {
                if let Some(active) = self.config.active.as_mut() {
                    active.project.copy_folder = value;
                }
                if let Err(e) = self.config.save_project() {
                    self.set_status(format!("Project: {e:#}"), true);
                }
            }
        }
    }

    fn reopen_audio(&mut self) {
        // Dropping the old player ends any stream it was playing at the old rate.
        let was_playing = self.is_playing();
        match Player::open(self.config.output_sample_rate, self.config.output_device.as_deref()) {
            Ok(p) => {
                self.player = Some(p);
                self.audio_error = None;
            }
            Err(e) => {
                self.player = None;
                self.audio_error = Some(format!("{e:#}"));
            }
        }
        // The selection and the fades outlive the reload.
        if let Some(cur) = &self.current {
            let (start, end) = self.wave.selection.map(|(a, b)| (Some(a), Some(b))).unwrap_or((None, None));
            self.open_region = Some((cur.path.clone(), crate::OpenRegion { start, end, fade_in: self.wave.fade_in, fade_out: self.wave.fade_out }));
        }
        // Everything decoded so far is at the old device rate.
        self.cache.clear();
        self.cache_order.clear();
        self.pending.clear();
        self.current = None;
        self.play_on_load = was_playing;
        if let Some(i) = self.selected {
            self.select(i, false);
        }
    }

    fn page_tags(&mut self, ui: &mut egui::Ui) {
        let theme = self.theme.clone();
        heading(ui, &theme, "Tags", "Shared by every project. The order is the 1 to 9 hotkey order; renaming rewrites the marks, removing drops them.");
        let used: std::collections::HashMap<String, usize> = self.db.tags_in_use().unwrap_or_default().into_iter().collect();
        let mut action: Option<TagAction> = None;
        let n = self.config.tags.len();
        card(ui, &theme, |ui| {
            for k in 0..n {
                if k > 0 {
                    ui.separator();
                }
                ui.add_space(6.0);
                let def = self.config.tags[k].clone();
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    ui.add_sized(egui::vec2(16.0, 28.0), egui::Label::new(RichText::new(if k < 9 { format!("{}", k + 1) } else { String::new() }).color(theme.muted).monospace()));
                    let mut c = theme.color_of(k, Some(&def));
                    if ui.color_edit_button_srgba(&mut c).changed() {
                        action = Some(TagAction::Color(k, theme::to_hex(c)));
                    }
                    let mut buf = self.tag_edit.as_ref().filter(|(i, _)| *i == k).map(|(_, s)| s.clone()).unwrap_or_else(|| def.name.clone());
                    let resp = ui.add(egui::TextEdit::singleline(&mut buf).desired_width(150.0));
                    if resp.has_focus() {
                        self.tag_edit = Some((k, buf.clone()));
                    }
                    if resp.lost_focus() {
                        action = Some(TagAction::Rename(k, buf.trim().to_string()));
                    }
                    let n_used = used.get(&def.name).copied().unwrap_or(0);
                    ui.label(RichText::new(format!("{n_used} file{}", if n_used == 1 { "" } else { "s" })).color(theme.muted).size(12.5));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        if theme::tool_button(ui, icons::TRASH, "").on_hover_text("Remove the tag and its marks").clicked() {
                            action = Some(TagAction::Remove(k));
                        }
                        if ui.add_enabled(k + 1 < n, theme::tool_button_widget(icons::CARET_DOWN, "")).clicked() {
                            action = Some(TagAction::Move(k, k + 1));
                        }
                        if ui.add_enabled(k > 0, theme::tool_button_widget(icons::CARET_UP, "")).clicked() {
                            action = Some(TagAction::Move(k, k - 1));
                        }
                    });
                });
                ui.add_space(6.0);
            }
            if n > 0 {
                ui.separator();
            }
            row(ui, &theme, true, "New tag", "Appears last; move it with the arrows.", |ui| {
                if theme::tool_button(ui, icons::PLUS, "Add").clicked() {
                    action = Some(TagAction::Add);
                }
            });
        });
        if let Some(a) = action {
            self.apply_tag_action(a);
        }
    }

    /// One row per rebindable action: click its key, press the new one. The key pressed is
    /// taken from this window's events, so the main window's handler never sees it.
    fn page_keys(&mut self, ui: &mut egui::Ui) {
        let theme = self.theme.clone();
        heading(ui, &theme, "Keys", "Click a key and press the new one, with any modifiers held; Escape keeps the old. The list's arrows, the tag digits and Ctrl+C are fixed.");
        if let Some(action) = self.key_capture {
            let got = ui.ctx().input(|i| i.events.iter().find_map(|e| match e {
                egui::Event::Key { key, pressed: true, modifiers, .. } => Some((*key, *modifiers)),
                _ => None,
            }));
            match got {
                Some((egui::Key::Escape, _)) => self.key_capture = None,
                Some((key, modifiers)) => {
                    if let Some(chord) = crate::keys::chord_of(key, modifiers) {
                        if let Err(e) = self.config.keys.set(action, &chord) {
                            self.set_status(format!("Key: {e}"), true);
                        }
                        let _ = self.config.save();
                        self.key_capture = None;
                    }
                }
                None => {}
            }
        }
        let mut reset_all = false;
        let mut set_capture: Option<Option<Action>> = None;
        let mut reset: Option<Action> = None;
        card(ui, &theme, |ui| {
            row(ui, &theme, true, "Defaults", "Every key back to what it started as.", |ui| {
                let b = egui::Button::new(theme::icon_text(icons::ARROW_COUNTER_CLOCKWISE, "Reset all", 14.0, egui::FontFamily::Proportional, Color32::PLACEHOLDER)).min_size(egui::vec2(0.0, 28.0));
                if ui.add_enabled(!self.config.keys.is_empty(), b).clicked() {
                    reset_all = true;
                }
            });
        });
        ui.add_space(12.0);
        card(ui, &theme, |ui| {
            for (n, action) in Action::ALL.into_iter().enumerate() {
                let chord = self.config.keys.chord(action).to_string();
                let capturing = self.key_capture == Some(action);
                let conflicts = self.config.keys.conflicts(action);
                let unknown = crate::keys::binding(&chord).is_none();
                row(ui, &theme, n == 0, action.label(), action.describe(), |ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    let (text, color) = match (capturing, unknown, conflicts.is_empty()) {
                        (true, _, _) => ("Press a key…".to_string(), theme.accent),
                        (_, true, _) => (chord.clone(), theme.err),
                        (_, _, false) => (crate::keys::hint(&chord), theme.warn),
                        _ => (crate::keys::hint(&chord), theme.text),
                    };
                    let b = egui::Button::new(RichText::new(text).family(egui::FontFamily::Name(theme::MEDIUM.into())).color(color)).selected(capturing).min_size(egui::vec2(96.0, 28.0));
                    let b = ui.add(b);
                    let b = if unknown {
                        b.on_hover_text("Not a key the window knows")
                    } else if !conflicts.is_empty() {
                        b.on_hover_text(format!("Also {}", conflicts.iter().map(|a| a.label()).collect::<Vec<_>>().join(", ")))
                    } else {
                        b
                    };
                    if b.clicked() {
                        set_capture = Some(if capturing { None } else { Some(action) });
                    }
                    if !self.config.keys.is_default(action) && theme::tool_button(ui, icons::ARROW_COUNTER_CLOCKWISE, "").on_hover_text(format!("Back to {}", crate::keys::hint(action.default_chord()))).clicked() {
                        reset = Some(action);
                    }
                });
            }
        });
        if let Some(c) = set_capture {
            self.key_capture = c;
        }
        if let Some(action) = reset {
            self.config.keys.reset(action);
            let _ = self.config.save();
        }
        if reset_all {
            self.config.keys.reset_all();
            let _ = self.config.save();
        }
    }

    fn page_presets(&mut self, ui: &mut egui::Ui) {
        let theme = self.theme.clone();
        heading(ui, &theme, "Presets", "What an export does to the sound and where it lands. The export card starts from one and can stray from it.");
        let mut save = false;
        let names: Vec<String> = self.config.presets.keys().cloned().collect();
        if !self.config.presets.contains_key(&self.preset_edit) {
            self.preset_edit = self.config.default_preset.clone();
        }
        card(ui, &theme, |ui| {
            row(ui, &theme, true, "Default preset", "Used by the CLI when none is given, and by the card at startup.", |ui| {
                egui::ComboBox::from_id_salt("default-preset").width(150.0).selected_text(&self.config.default_preset).show_ui(ui, |ui| {
                    for name in &names {
                        if ui.selectable_value(&mut self.config.default_preset, name.clone(), name).clicked() {
                            save = true;
                        }
                    }
                });
            });
        });
        ui.add_space(theme::GAP);

        let mut add: Option<Option<String>> = None;
        let mut remove = false;
        ui.horizontal_wrapped(|ui| {
            for name in &names {
                let on = *name == self.preset_edit;
                let b = egui::Button::new(RichText::new(name).size(13.5)).selected(on).min_size(egui::vec2(0.0, 28.0)).fill(if on { theme.select } else { theme.surface });
                if ui.add(b).clicked() {
                    self.preset_edit = name.clone();
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if theme::tool_button(ui, icons::PLUS, "New").on_hover_text("A preset with the default values").clicked() {
                    add = Some(None);
                }
                if theme::tool_button(ui, icons::COPY, "Duplicate").clicked() {
                    add = Some(Some(self.preset_edit.clone()));
                }
            });
        });
        ui.add_space(8.0);

        let editing = self.preset_edit.clone();
        let mut rename: Option<String> = None;
        let Some(mut p) = self.config.presets.get(&editing).cloned() else { return };
        let before = p.clone();
        card(ui, &theme, |ui| {
            row(ui, &theme, true, "Name", "The default follows the preset through a rename.", |ui| {
                let mut buf = self.preset_rename.clone().unwrap_or_else(|| editing.clone());
                let resp = ui.add(egui::TextEdit::singleline(&mut buf).desired_width(180.0));
                if resp.has_focus() {
                    self.preset_rename = Some(buf.clone());
                }
                if resp.lost_focus() {
                    self.preset_rename = None;
                    rename = Some(buf.trim().to_string());
                }
            });
            row(ui, &theme, false, "Format", "WAV or FLAC, and the bit depth. FLAC stops at 24 bit.", |ui| {
                preset_ui::dither(ui, &mut p);
                preset_ui::bits(ui, "settings", &mut p);
                preset_ui::format(ui, "settings", &mut p);
            });
            row(ui, &theme, false, "Sample rate", "Resampled on export; source rate keeps whatever the file has.", |ui| {
                preset_ui::sample_rate(ui, "settings", &mut p);
            });
            row(ui, &theme, false, "Channels", "Keep the source layout, or fold it down to one channel.", |ui| {
                preset_ui::channels(ui, "settings", &mut p);
            });
            row(ui, &theme, false, "Normalise", "Peak scales to a sample ceiling; LUFS matches integrated loudness under a true-peak ceiling.", |ui| {
                ui.horizontal(|ui| preset_ui::normalize(ui, "settings", &mut p));
            });
            row(ui, &theme, false, "Fade shape", "The curve of the selection's fades; the transport can override it per export.", |ui| {
                preset_ui::fade_shape(ui, "settings", &mut p);
            });
            row(ui, &theme, false, "Path template", "The file name, and any folders, inside the export folder. Hover the field for the placeholders.", |ui| {
                preset_ui::path_template(ui, &mut p, 260.0);
            });
            row(ui, &theme, false, "Remove preset", "The default moves to another preset.", |ui| {
                let b = egui::Button::new(theme::icon_text(icons::TRASH, "Remove", 14.0, egui::FontFamily::Proportional, theme.err)).min_size(egui::vec2(0.0, 28.0));
                if ui.add_enabled(names.len() > 1, b).clicked() {
                    remove = true;
                }
            });
        });
        if p != before {
            self.config.presets.insert(editing.clone(), p.clone());
            if self.preset_name.as_deref() == Some(&editing) {
                self.preset = p;
            }
            save = true;
        }
        if let Some(new) = rename {
            if new != editing && self.config.rename_preset(&editing, &new).is_ok() {
                if self.preset_name.as_deref() == Some(&editing) {
                    self.preset_name = Some(new.clone());
                }
                self.preset_edit = new;
                save = true;
            }
        }
        if let Some(from) = add {
            let (base, values) = match from {
                Some(src) => (src.clone(), self.config.presets.get(&src).cloned().unwrap_or_default()),
                None => ("preset".to_string(), hark_core::ExportPreset::default()),
            };
            let name = self.config.free_preset_name(&base);
            self.config.presets.insert(name.clone(), values);
            self.preset_edit = name;
            save = true;
        }
        if remove && self.config.remove_preset(&editing).is_ok() {
            if self.preset_name.as_deref() == Some(&editing) {
                self.preset_name = None;
            }
            self.preset_edit = self.config.default_preset.clone();
            save = true;
        }
        if save {
            let _ = self.config.save();
        }
    }
}

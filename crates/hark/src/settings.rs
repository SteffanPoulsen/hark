//! The settings window: a page list on the left, and on the right cards of rows that pair a
//! title and a one-line explanation with the control that changes it.

use egui::{Color32, CornerRadius, Margin, RichText};
use egui_phosphor::regular as icons;
use hark_core::audition::Player;

use crate::theme::{self, Theme};
use crate::preset_ui;
use crate::{App, TagAction};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Page {
    General,
    Projects,
    Tags,
    Presets,
}

impl Page {
    const ALL: [Page; 4] = [Page::General, Page::Projects, Page::Tags, Page::Presets];

    fn title(self) -> &'static str {
        match self {
            Page::General => "General",
            Page::Projects => "Projects",
            Page::Tags => "Tags",
            Page::Presets => "Presets",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Page::General => icons::SLIDERS_HORIZONTAL,
            Page::Projects => icons::CUBE,
            Page::Tags => icons::TAG,
            Page::Presets => icons::SLIDERS,
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
            return;
        }
        // A real window of its own, so the compositor tiles it like any other.
        let builder = egui::ViewportBuilder::default().with_title("hark settings").with_app_id("hark").with_inner_size([960.0, 720.0]).with_min_inner_size([640.0, 420.0]);
        ctx.show_viewport_immediate(egui::ViewportId::from_hash_of("settings"), builder, |ctx, _| {
            if ctx.input(|i| i.viewport().close_requested()) {
                self.settings_open = false;
                self.tag_edit = None;
                self.project_edit = None;
                self.preset_rename = None;
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
                            Page::Projects => self.page_projects(ui),
                            Page::Tags => self.page_tags(ui),
                            Page::Presets => self.page_presets(ui),
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
                    let zoom = ui.add(egui::Slider::new(&mut self.config.ui_zoom, 0.8..=2.0).step_by(0.05).fixed_decimals(2));
                    if zoom.changed() {
                        ui.ctx().set_zoom_factor(self.config.ui_zoom);
                    }
                    save |= zoom.drag_stopped() || zoom.lost_focus();
                });
            });
            row(ui, &theme, false, "Audition sample rate", "The rate the output device is opened at; sources are resampled to it for playback.", |ui| {
                let current = self.config.audition_sample_rate;
                let label = |r: Option<u32>| r.map(|r| format!("{:.1} kHz", r as f64 / 1000.0)).unwrap_or_else(|| "device default".into());
                egui::ComboBox::from_id_salt("audition-rate").width(150.0).selected_text(label(current)).show_ui(ui, |ui| {
                    let mut pick = |ui: &mut egui::Ui, r: Option<u32>| {
                        if ui.selectable_label(current == r, label(r)).clicked() && current != r {
                            self.config.audition_sample_rate = r;
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
            let device = match (&self.player, &self.audio_error) {
                (Some(p), _) => format!("{}  ·  {} Hz  ·  {} ch", p.device_name, p.sample_rate, p.channels),
                (None, Some(e)) => format!("no audio: {e}"),
                _ => "no audio".into(),
            };
            row(ui, &theme, false, "Audio output", &device, |_| {});
            let palette = self.config.theme_file.as_ref().map(|p| p.display().to_string());
            row(ui, &theme, false, "Theme palette", "A palette file in the desktop theme format replaces the built-in colors.", |ui| {
                if palette.is_some() && theme::tool_button(ui, icons::X, "").on_hover_text("use the built-in palette").clicked() {
                    self.config.theme_file = None;
                    save = true;
                    retheme = true;
                }
                let label = palette.as_deref().and_then(|p| p.rsplit('/').next().map(str::to_string)).unwrap_or_else(|| "choose…".into());
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

    fn reopen_audio(&mut self) {
        match Player::open(self.config.audition_sample_rate) {
            Ok(p) => {
                self.player = Some(p);
                self.audio_error = None;
            }
            Err(e) => {
                self.player = None;
                self.audio_error = Some(format!("{e:#}"));
            }
        }
        // Everything decoded so far is at the old device rate.
        self.cache.clear();
        self.cache_order.clear();
        self.pending.clear();
        self.current = None;
        if let Some(i) = self.selected {
            self.select(i, false);
        }
    }

    fn page_projects(&mut self, ui: &mut egui::Ui) {
        let theme = self.theme.clone();
        heading(ui, &theme, "Projects", "A project owns an output folder, a library scope and its own favorites. Tags and presets are shared.");
        let names: Vec<String> = self.config.projects.iter().map(|p| p.name.clone()).collect();
        let active = self.config.active_project.clone();
        let mut switch: Option<String> = None;
        let mut new_project = false;
        card(ui, &theme, |ui| {
            row(ui, &theme, true, "Active project", "The one the window and the CLI work in.", |ui| {
                if theme::tool_button(ui, icons::PLUS, "new").clicked() {
                    new_project = true;
                }
                egui::ComboBox::from_id_salt("settings-project").width(180.0).selected_text(&active).show_ui(ui, |ui| {
                    for n in &names {
                        if ui.selectable_label(*n == active, n).clicked() {
                            switch = Some(n.clone());
                        }
                    }
                });
            });
        });
        if new_project {
            self.new_project();
        }
        if let Some(n) = switch {
            self.switch_project(&n);
        }
        ui.add_space(theme::GAP);

        let favs = self.db.favorite_count(&active).unwrap_or(0);
        let mut save = false;
        let mut rename: Option<String> = None;
        let mut remove_scope: Option<usize> = None;
        let mut add_scope = false;
        let mut remove_project = false;
        card(ui, &theme, |ui| {
            row(ui, &theme, true, "Name", "Favorites follow the project through a rename.", |ui| {
                let mut buf = self.project_edit.clone().unwrap_or_else(|| active.clone());
                let resp = ui.add(egui::TextEdit::singleline(&mut buf).desired_width(180.0));
                if resp.has_focus() {
                    self.project_edit = Some(buf.clone());
                }
                if resp.lost_focus() {
                    self.project_edit = None;
                    rename = Some(buf.trim().to_string());
                }
            });
            let out = self.config.output_root().map(|p| p.display().to_string());
            row(ui, &theme, false, "Output folder", "Exports land here, under the preset's path template.", |ui| {
                let label = out.as_deref().unwrap_or("choose…");
                let b = ui.add(egui::Button::new(theme::icon_text(icons::FOLDER, label, 14.0, egui::FontFamily::Proportional, Color32::PLACEHOLDER)).min_size(egui::vec2(0.0, 28.0)).truncate());
                let b = match &out {
                    Some(o) => b.on_hover_text(o),
                    None => b,
                };
                if b.clicked() {
                    if let Some(path) = rfd::FileDialog::new().pick_folder() {
                        self.config.project_mut().output_root = Some(path);
                        save = true;
                    }
                }
            });
            row(ui, &theme, false, "Favorites", &format!("{favs} file{} starred in this project.", if favs == 1 { "" } else { "s" }), |_| {});
            row(ui, &theme, false, "Scope", "Library folders this project cares about. Empty means the whole library.", |ui| {
                if theme::tool_button(ui, icons::FOLDER_PLUS, "add folder").clicked() {
                    add_scope = true;
                }
            });
            let scope = self.config.project().scope.clone();
            for (i, p) in scope.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.add_space(16.0);
                    ui.label(theme::icon(icons::FOLDER).color(theme.muted));
                    ui.add(egui::Label::new(RichText::new(p.display().to_string()).size(13.0)).truncate());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if theme::tool_button(ui, icons::X, "").on_hover_text("remove from the scope").clicked() {
                            remove_scope = Some(i);
                        }
                    });
                });
                ui.add_space(4.0);
            }
            if !scope.is_empty() {
                ui.add_space(4.0);
            }
            row(ui, &theme, false, "Remove project", "Drops its favorites. Exports and the library stay.", |ui| {
                let b = egui::Button::new(theme::icon_text(icons::TRASH, "remove", 14.0, egui::FontFamily::Proportional, theme.err)).min_size(egui::vec2(0.0, 28.0));
                if ui.add_enabled(self.config.projects.len() > 1, b).clicked() {
                    remove_project = true;
                }
            });
        });
        if let Some(new) = rename {
            if !new.is_empty() && new != active && !self.config.projects.iter().any(|p| p.name == new) {
                match self.db.rename_project(&active, &new) {
                    Ok(_) => {
                        self.config.project_mut().name = new.clone();
                        self.config.active_project = new.clone();
                        self.db.set_project(&new);
                        save = true;
                    }
                    Err(e) => self.set_status(format!("rename project: {e:#}"), true),
                }
            }
        }
        if let Some(i) = remove_scope {
            self.config.project_mut().scope.remove(i);
            self.expanded = self.config.scope_folders().into_iter().map(|(_, p)| p).collect();
            self.list_dirty = true;
            save = true;
        }
        if add_scope {
            let mut dialog = rfd::FileDialog::new();
            if let Some(r) = self.config.roots.first() {
                dialog = dialog.set_directory(&r.path);
            }
            if let Some(path) = dialog.pick_folder() {
                if !self.config.project().scope.contains(&path) {
                    self.config.project_mut().scope.push(path.clone());
                    self.expanded.insert(path);
                    self.list_dirty = true;
                    save = true;
                }
            }
        }
        if save {
            let _ = self.config.save();
        }
        if remove_project {
            let i = self.config.projects.iter().position(|p| p.name == active).unwrap_or(0);
            self.config.projects.remove(i);
            let _ = self.db.forget_project(&active);
            let next = self.config.projects[0].name.clone();
            self.switch_project(&next);
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
                        if theme::tool_button(ui, icons::TRASH, "").on_hover_text("remove the tag and its marks").clicked() {
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
                if theme::tool_button(ui, icons::PLUS, "add").clicked() {
                    action = Some(TagAction::Add);
                }
            });
        });
        if let Some(a) = action {
            self.apply_tag_action(a);
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
            row(ui, &theme, true, "Default preset", "Used for files whose tags select no preset.", |ui| {
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
                if theme::tool_button(ui, icons::PLUS, "new").on_hover_text("a preset with the default values").clicked() {
                    add = Some(None);
                }
                if theme::tool_button(ui, icons::COPY, "duplicate").clicked() {
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
            row(ui, &theme, true, "Name", "Tags and the default follow the preset through a rename.", |ui| {
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
            row(ui, &theme, false, "Trim silence", "Cuts leading and trailing silence below the threshold, keeping a pad around the sound.", |ui| {
                ui.horizontal(|ui| preset_ui::trim_silence(ui, &mut p));
            });
            row(ui, &theme, false, "Fade shape", "The curve of the selection's fades; the transport can override it per export.", |ui| {
                preset_ui::fade_shape(ui, "settings", &mut p);
            });
            row(ui, &theme, false, "Path template", "Where the file lands under the output folder. Hover the field for the placeholders.", |ui| {
                preset_ui::path_template(ui, &mut p, 260.0);
            });
            row(ui, &theme, false, "Remove preset", "Tags that selected it select none; the default moves to another preset.", |ui| {
                let b = egui::Button::new(theme::icon_text(icons::TRASH, "remove", 14.0, egui::FontFamily::Proportional, theme.err)).min_size(egui::vec2(0.0, 28.0));
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

//! Palette and egui styling. The built-in palette is a flat dark theme; a palette file in the
//! desktop-theme format (`{"bg": "#14121B", "accent": "#1AD1D1", ...}`) overrides it.

use std::collections::HashMap;
use std::path::Path;

use egui::{Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, Stroke, TextStyle, Visuals};

#[derive(Clone, Debug)]
pub struct Theme {
    pub bg_deep: Color32,
    pub bg: Color32,
    pub surface: Color32,
    pub overlay: Color32,
    pub select: Color32,
    pub line: Color32,
    pub muted: Color32,
    pub text: Color32,
    /// The brand color: logo, focus, the export button, the waveform, stars.
    pub accent: Color32,
    pub accent_soft: Color32,
    /// The second accent: every selected thing (rows, toggled buttons, the waveform region).
    pub accent2: Color32,
    pub accent2_soft: Color32,
    pub ok: Color32,
    pub warn: Color32,
    pub err: Color32,
}

/// Inner margin of every panel, in points.
pub const PAD: i8 = 12;
/// Space between groups of controls.
pub const GAP: f32 = 14.0;
/// Inner margin of a card.
pub const CARD_PAD: i8 = 16;
/// Height of a row in the tree and the list.
pub const ROW_H: f32 = 26.0;

pub const SEMIBOLD: &str = "inter-semibold";
pub const MEDIUM: &str = "inter-medium";
pub const ICONS: &str = "icons";
pub const ICONS_FILL: &str = "phosphor-fill";

pub fn hex(s: &str) -> Option<Color32> {
    hark_core::tag_color::parse(s).map(|[r, g, b]| Color32::from_rgb(r, g, b))
}

impl Default for Theme {
    fn default() -> Self {
        Theme {
            bg_deep: hex("#161616").unwrap(),
            bg: hex("#1e1e1e").unwrap(),
            surface: hex("#242424").unwrap(),
            overlay: hex("#2c2c2c").unwrap(),
            select: hex("#383838").unwrap(),
            line: hex("#3a3a3a").unwrap(),
            muted: hex("#9a9a9a").unwrap(),
            text: hex("#dcddde").unwrap(),
            accent: hex("#d98a56").unwrap(),
            accent_soft: hex("#3d2a1f").unwrap(),
            accent2: hex("#5fbfa8").unwrap(),
            accent2_soft: hex("#1c3a32").unwrap(),
            ok: hex("#6fbf73").unwrap(),
            warn: hex("#e8c75c").unwrap(),
            err: hex("#f05a5a").unwrap(),
        }
    }
}

impl Theme {
    pub fn from_file(path: &Path) -> Option<Theme> {
        let text = std::fs::read_to_string(path).ok()?;
        let map: HashMap<String, serde_json::Value> = serde_json::from_str(&text).ok()?;
        let get = |k: &str| map.get(k).and_then(|v| v.as_str()).and_then(hex);
        let d = Theme::default();
        Some(Theme {
            bg_deep: get("bg_deep").unwrap_or(d.bg_deep),
            bg: get("bg").unwrap_or(d.bg),
            surface: get("surface").unwrap_or(d.surface),
            overlay: get("overlay").unwrap_or(d.overlay),
            select: get("select").unwrap_or(d.select),
            line: get("line").unwrap_or(d.line),
            muted: get("muted").unwrap_or(d.muted),
            text: get("text").unwrap_or(d.text),
            accent: get("accent").unwrap_or(d.accent),
            accent_soft: get("accent_soft").unwrap_or(d.accent_soft),
            accent2: get("accent2").unwrap_or(d.accent2),
            accent2_soft: get("accent2_soft").unwrap_or(d.accent2_soft),
            ok: get("ok").unwrap_or(d.ok),
            warn: get("warn").unwrap_or(d.warn),
            err: get("err").unwrap_or(d.err),
        })
    }

    /// Text on an accent fill: dark on a light accent, white on a dark one.
    pub fn on_accent(&self) -> Color32 {
        let c = self.accent;
        let luma = 0.2126 * c.r() as f32 + 0.7152 * c.g() as f32 + 0.0722 * c.b() as f32;
        if luma > 140.0 { self.bg_deep } else { Color32::WHITE }
    }

    /// A tag's color: its own, or grey for a tag on a file that is no longer defined.
    pub fn color_of(&self, def: Option<&hark_core::TagDef>) -> Color32 {
        def.and_then(|d| d.color.as_deref()).and_then(hex).unwrap_or(self.muted)
    }

    pub fn apply(&self, ctx: &egui::Context) {
        let mut fonts = FontDefinitions::default();
        fonts.font_data.insert("inter".into(), FontData::from_static(include_bytes!("../assets/fonts/Inter-Regular.ttf")).into());
        fonts.font_data.insert(MEDIUM.into(), FontData::from_static(include_bytes!("../assets/fonts/Inter-Medium.ttf")).into());
        fonts.font_data.insert(SEMIBOLD.into(), FontData::from_static(include_bytes!("../assets/fonts/Inter-SemiBold.ttf")).into());
        // Inter maps its stylistic alternates into the private-use area and Phosphor defines blank
        // glyphs for letters, so neither can sit in the other's family: icons get their own.
        egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
        egui_phosphor::add_font_bytes_as_family(&mut fonts, ICONS_FILL, egui_phosphor::Variant::Fill.font_bytes());
        fonts.families.insert(FontFamily::Proportional, vec!["inter".into(), "Ubuntu-Light".into()]);
        fonts.families.insert(FontFamily::Name(MEDIUM.into()), vec![MEDIUM.into()]);
        fonts.families.insert(FontFamily::Name(SEMIBOLD.into()), vec![SEMIBOLD.into()]);
        // A family takes its line metrics from its first font, so each icon family holds its
        // Phosphor variant alone and a filled icon sits where its outline does.
        fonts.families.insert(FontFamily::Name(ICONS.into()), vec!["phosphor".into()]);
        fonts.families.insert(FontFamily::Name(ICONS_FILL.into()), vec![ICONS_FILL.into()]);
        ctx.set_fonts(fonts);

        let mut v = Visuals::dark();
        v.override_text_color = Some(self.text);
        v.weak_text_color = Some(self.muted);
        v.panel_fill = self.bg;
        v.window_fill = self.surface;
        v.window_stroke = Stroke::new(1.0, self.line);
        v.window_corner_radius = CornerRadius::same(8);
        v.menu_corner_radius = CornerRadius::same(6);
        v.extreme_bg_color = self.bg_deep;
        v.faint_bg_color = self.surface;
        v.text_edit_bg_color = Some(self.bg_deep);
        v.code_bg_color = self.bg_deep;
        v.hyperlink_color = self.accent;
        v.warn_fg_color = self.warn;
        v.error_fg_color = self.err;
        v.selection.bg_fill = self.accent2_soft;
        v.selection.stroke = Stroke::new(1.0, self.accent2);
        v.text_cursor.stroke = Stroke::new(2.0, self.accent2);
        v.striped = true;
        v.slider_trailing_fill = true;
        v.handle_shape = egui::style::HandleShape::Circle;
        v.window_shadow = egui::epaint::Shadow { offset: [0, 4], blur: 16, spread: 0, color: Color32::from_black_alpha(120) };
        v.popup_shadow = v.window_shadow;

        let radius = CornerRadius::same(5);
        let w = &mut v.widgets;
        w.noninteractive.bg_fill = self.bg;
        w.noninteractive.weak_bg_fill = self.bg;
        w.noninteractive.bg_stroke = Stroke::new(1.0, self.line);
        w.noninteractive.fg_stroke = Stroke::new(1.0, self.muted);
        w.noninteractive.corner_radius = radius;
        w.inactive.bg_fill = self.overlay;
        w.inactive.weak_bg_fill = self.overlay;
        w.inactive.bg_stroke = Stroke::NONE;
        w.inactive.fg_stroke = Stroke::new(1.0, self.text);
        w.inactive.corner_radius = radius;
        w.hovered.bg_fill = self.select;
        w.hovered.weak_bg_fill = self.select;
        w.hovered.bg_stroke = Stroke::new(1.0, self.line);
        w.hovered.fg_stroke = Stroke::new(1.0, self.text);
        w.hovered.corner_radius = radius;
        w.hovered.expansion = 0.0;
        w.active.bg_fill = self.accent2_soft;
        w.active.weak_bg_fill = self.accent2_soft;
        w.active.bg_stroke = Stroke::new(1.0, self.accent2);
        w.active.fg_stroke = Stroke::new(1.0, self.text);
        w.active.corner_radius = radius;
        w.active.expansion = 0.0;
        w.open.bg_fill = self.select;
        w.open.weak_bg_fill = self.select;
        w.open.bg_stroke = Stroke::new(1.0, self.line);
        w.open.fg_stroke = Stroke::new(1.0, self.text);
        w.open.corner_radius = radius;

        ctx.all_styles_mut(|s| {
            s.visuals = v.clone();
            s.text_styles = [
                (TextStyle::Small, FontId::new(11.0, FontFamily::Proportional)),
                (TextStyle::Body, FontId::new(14.0, FontFamily::Proportional)),
                (TextStyle::Button, FontId::new(14.0, FontFamily::Proportional)),
                (TextStyle::Heading, FontId::new(17.0, FontFamily::Name(SEMIBOLD.into()))),
                (TextStyle::Monospace, FontId::new(13.0, FontFamily::Monospace)),
            ]
            .into();
            s.spacing.item_spacing = egui::vec2(8.0, 6.0);
            s.spacing.button_padding = egui::vec2(10.0, 5.0);
            s.spacing.interact_size = egui::vec2(40.0, 24.0);
            s.spacing.window_margin = Margin::same(12);
            s.spacing.menu_margin = Margin::symmetric(6, 8);
            s.spacing.indent = 16.0;
            s.spacing.icon_width = 18.0;
            s.spacing.icon_width_inner = 10.0;
            s.spacing.icon_spacing = 8.0;
            s.spacing.slider_width = 120.0;
            s.interaction.selectable_labels = false;
        });
    }
}

pub fn semibold(text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(text).family(FontFamily::Name(SEMIBOLD.into()))
}


pub fn icon_fill(icon: &str) -> egui::RichText {
    egui::RichText::new(icon).family(FontFamily::Name(ICONS_FILL.into()))
}

pub fn to_hex(c: Color32) -> String {
    format!("#{:02x}{:02x}{:02x}", c.r(), c.g(), c.b())
}

pub fn icon(icon: &str) -> egui::RichText {
    egui::RichText::new(icon).family(FontFamily::Name(ICONS.into()))
}

/// A button of the standard control height with an icon and, when `label` is not empty, text.
pub fn tool_button_widget(icon: &str, label: &str) -> egui::Button<'static> {
    let text: egui::WidgetText = if label.is_empty() {
        egui::RichText::new(icon).family(FontFamily::Name(ICONS.into())).size(15.0).into()
    } else {
        icon_text(icon, label, 14.0, FontFamily::Proportional, Color32::PLACEHOLDER).into()
    };
    egui::Button::new(text).min_size(egui::vec2(30.0, 28.0))
}

/// The inset every row of a menu shares, so buttons, checks and headings start on one line.
pub const MENU_INSET: f32 = 10.0;

/// egui gives menu buttons a two-pixel padding; a menu of ours pads its rows alike.
pub fn menu_items(ui: &mut egui::Ui) {
    ui.spacing_mut().button_padding = egui::vec2(MENU_INSET, 5.0);
    ui.spacing_mut().item_spacing.y = 2.0;
}

/// A check box on a menu row: the row highlights across its width like a button's, with
/// the box and text inset like a button's text.
pub fn menu_check(ui: &mut egui::Ui, on: &mut bool, text: impl Into<egui::WidgetText>) -> egui::Response {
    ui.add(Check::new(on, text).menu_row())
}

/// A muted heading on a menu row.
pub fn menu_heading(ui: &mut egui::Ui, theme: &Theme, text: &str) {
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.add_space(MENU_INSET);
        ui.label(egui::RichText::new(text).color(theme.muted).size(12.0));
    });
}

/// A check box drawn to be seen: a bordered square, filled with the accent and ticked when on.
pub struct Check<'a> {
    on: &'a mut bool,
    text: egui::WidgetText,
    menu_row: bool,
}

impl<'a> Check<'a> {
    pub fn new(on: &'a mut bool, text: impl Into<egui::WidgetText>) -> Self {
        Check { on, text: text.into(), menu_row: false }
    }

    pub fn menu_row(mut self) -> Self {
        self.menu_row = true;
        self
    }
}

/// What a menu check row adds to its text's width: the box, the gap and the insets.
pub const CHECK_ROW_W: f32 = CHECK_BOX + CHECK_GAP + 2.0 * MENU_INSET;
const CHECK_BOX: f32 = 18.0;
const CHECK_GAP: f32 = 8.0;

impl egui::Widget for Check<'_> {
    fn ui(self, ui: &mut egui::Ui) -> egui::Response {
        const BOX: f32 = CHECK_BOX;
        const GAP: f32 = CHECK_GAP;
        let galley = self.text.into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, TextStyle::Button);
        let text_w = if galley.is_empty() { 0.0 } else { galley.size().x + GAP };
        let inset = if self.menu_row { MENU_INSET } else { 0.0 };
        let content_h = BOX.max(galley.size().y).max(ui.spacing().interact_size.y);
        let content_w = BOX + text_w + 2.0 * inset;
        // A menu sizes itself to its content in a sizing pass, then its rows fill the width.
        let fill = self.menu_row && !ui.is_sizing_pass();
        let size = egui::vec2(if fill { ui.available_width().max(content_w) } else { content_w }, content_h + if self.menu_row { 2.0 * ui.spacing().button_padding.y } else { 0.0 });
        let (rect, mut response) = ui.allocate_exact_size(size, egui::Sense::click());
        if response.clicked() {
            *self.on = !*self.on;
            response.mark_changed();
        }
        if ui.is_rect_visible(rect) {
            let visuals = ui.style().interact(&response);
            if self.menu_row && (response.hovered() || response.has_focus()) {
                ui.painter().rect_filled(rect, visuals.corner_radius, visuals.weak_bg_fill);
            }
            let accent = ui.visuals().selection.stroke.color;
            let square = egui::Rect::from_center_size(egui::pos2(rect.left() + inset + BOX / 2.0, rect.center().y), egui::Vec2::splat(BOX));
            let (fill, stroke) = if *self.on {
                let fill = if response.hovered() { accent.gamma_multiply(1.2).to_opaque() } else { accent };
                (fill, Stroke::new(1.0, fill))
            } else {
                (visuals.bg_fill, Stroke::new(1.0, if response.hovered() { visuals.fg_stroke.color } else { ui.visuals().widgets.noninteractive.bg_stroke.color }))
            };
            ui.painter().rect(square, CornerRadius::same(4), fill, stroke, egui::StrokeKind::Inside);
            if *self.on {
                let c = square.center();
                let tick = [egui::pos2(c.x - 4.5, c.y + 0.5), egui::pos2(c.x - 1.5, c.y + 3.5), egui::pos2(c.x + 4.5, c.y - 3.5)];
                let on = { let luma = 0.2126 * accent.r() as f32 + 0.7152 * accent.g() as f32 + 0.0722 * accent.b() as f32; if luma > 140.0 { Color32::from_rgb(0x16, 0x16, 0x16) } else { Color32::WHITE } };
                ui.painter().add(egui::Shape::line(tick.to_vec(), Stroke::new(2.0, on)));
            }
            if !galley.is_empty() {
                let pos = egui::pos2(square.right() + GAP, rect.center().y - galley.size().y / 2.0);
                ui.painter().galley(pos, galley, visuals.text_color());
            }
        }
        response
    }
}

pub fn tool_button(ui: &mut egui::Ui, icon: &str, label: &str) -> egui::Response {
    ui.add(tool_button_widget(icon, label))
}

/// An icon followed by text in one label. `Color32::PLACEHOLDER` takes the widget's text color.
/// Text with an icon after it.
pub fn text_icon(text: &str, icon: &str, size: f32, color: Color32) -> egui::text::LayoutJob {
    use egui::text::{LayoutJob, TextFormat};
    let mut job = LayoutJob::default();
    job.append(text, 0.0, TextFormat { font_id: FontId::new(size, FontFamily::Proportional), color, valign: egui::Align::Center, ..Default::default() });
    job.append(icon, size * 0.3, TextFormat { font_id: FontId::new(size, FontFamily::Name(ICONS.into())), color, valign: egui::Align::Center, ..Default::default() });
    job
}

pub fn icon_text(icon: &str, text: &str, size: f32, family: FontFamily, color: Color32) -> egui::text::LayoutJob {
    use egui::text::{LayoutJob, TextFormat};
    let mut job = LayoutJob::default();
    job.append(icon, 0.0, TextFormat { font_id: FontId::new(size, FontFamily::Name(ICONS.into())), color, valign: egui::Align::Center, ..Default::default() });
    if !text.is_empty() {
        job.append(text, size * 0.5, TextFormat { font_id: FontId::new(size, family), color, valign: egui::Align::Center, ..Default::default() });
    }
    job
}

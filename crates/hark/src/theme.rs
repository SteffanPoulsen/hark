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
    /// The cool counterpart for the selection and fades over the waveform.
    pub accent2: Color32,
    pub ok: Color32,
    pub warn: Color32,
    pub err: Color32,
    pub tag_colors: Vec<Color32>,
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
    let s = s.trim().trim_start_matches('#');
    if s.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(s, 16).ok()?;
    Some(Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8))
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
            accent: hex("#e3a447").unwrap(),
            accent_soft: hex("#3d3119").unwrap(),
            accent2: hex("#5aa9d6").unwrap(),
            ok: hex("#6fbf73").unwrap(),
            warn: hex("#e8c75c").unwrap(),
            err: hex("#f05a5a").unwrap(),
            tag_colors: Self::default_tag_colors(),
        }
    }
}

impl Theme {
    fn default_tag_colors() -> Vec<Color32> {
        ["#e06c75", "#e5a44c", "#d7c653", "#79b86a", "#4fb5c0", "#5f8fe8", "#a77cf2", "#e078c0", "#8f8f8f"].iter().filter_map(|h| hex(h)).collect()
    }

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
            ok: get("ok").unwrap_or(d.ok),
            warn: get("warn").unwrap_or(d.warn),
            err: get("err").unwrap_or(d.err),
            tag_colors: Self::default_tag_colors(),
        })
    }

    pub fn tag_color(&self, index: usize) -> Color32 {
        self.tag_colors[index % self.tag_colors.len()]
    }

    /// A tag's color: its own when set, else one picked by its position.
    pub fn color_of(&self, index: usize, def: Option<&hark_core::TagDef>) -> Color32 {
        def.and_then(|d| d.color.as_deref()).and_then(hex).unwrap_or_else(|| self.tag_color(index))
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
        fonts.families.insert(FontFamily::Name(ICONS.into()), vec!["phosphor".into()]);
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
        v.selection.bg_fill = self.accent_soft;
        v.selection.stroke = Stroke::new(1.0, self.accent);
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
        w.active.bg_fill = self.accent_soft;
        w.active.weak_bg_fill = self.accent_soft;
        w.active.bg_stroke = Stroke::new(1.0, self.accent);
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
            s.spacing.menu_margin = Margin::same(8);
            s.spacing.indent = 16.0;
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

pub fn tool_button(ui: &mut egui::Ui, icon: &str, label: &str) -> egui::Response {
    ui.add(tool_button_widget(icon, label))
}

/// An icon followed by text in one label. `Color32::PLACEHOLDER` takes the widget's text color.
pub fn icon_text(icon: &str, text: &str, size: f32, family: FontFamily, color: Color32) -> egui::text::LayoutJob {
    use egui::text::{LayoutJob, TextFormat};
    let mut job = LayoutJob::default();
    job.append(icon, 0.0, TextFormat { font_id: FontId::new(size, FontFamily::Name(ICONS.into())), color, valign: egui::Align::Center, ..Default::default() });
    if !text.is_empty() {
        job.append(text, size * 0.5, TextFormat { font_id: FontId::new(size, family), color, valign: egui::Align::Center, ..Default::default() });
    }
    job
}

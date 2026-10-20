//! What changed in this version: a card over the window on the first start after an update,
//! opened again from the About page.

use std::f32::consts::TAU;

use egui::epaint::{PathShape, PathStroke};
use egui::text::{LayoutJob, TextFormat};
use egui::{Align2, Color32, CornerRadius, FontFamily, FontId, Margin, Pos2, Rect, RichText, Sense, Stroke};
use egui_phosphor::regular as icons;
use kuulo_core::changelog::Notes;

use crate::theme::{self, Theme};
use crate::App;

/// Where the window keeps the last version that ran it.
pub const SEEN_KEY: &str = "last_version";

const CARD_W: f32 = 560.0;
const ICON: f32 = 60.0;
/// The wave starts tracing itself across the card once the card has been up this long, so
/// the eye has found it, and takes `TRACE_SECS` to cross.
const TRACE_DELAY_SECS: f64 = 0.35;
const TRACE_SECS: f64 = 2.0;

pub struct WhatsNew {
    /// "Updated from 0.5.4" under the lockup, after an update.
    headline: Option<String>,
    releases: Vec<Notes>,
    /// When the card first drew, which starts the wave's trace.
    shown_at: Option<f64>,
}

/// What to show on this start: the releases since the version that last ran the window. A
/// window that ran before kuulo kept that version gets this version's notes; a fresh install
/// gets none.
pub fn on_start(seen: Option<&str>, ran_before: bool) -> Option<WhatsNew> {
    let releases = match seen {
        Some(seen) => kuulo_core::changelog::since(seen),
        None if ran_before => kuulo_core::changelog::this_version().into_iter().collect(),
        None => Vec::new(),
    };
    let headline = seen.map_or_else(|| "Updated".to_string(), |seen| format!("Updated from {seen}"));
    (!releases.is_empty()).then_some(WhatsNew { headline: Some(headline), releases, shown_at: None })
}

/// This version's card, opened from the About page.
pub fn again() -> Option<WhatsNew> {
    kuulo_core::changelog::this_version().map(|notes| WhatsNew { headline: None, releases: vec![notes], shown_at: None })
}

/// kuulo's lockup with this version, as on the social card, and the headline under it.
fn lockup(ui: &mut egui::Ui, theme: &Theme, headline: Option<&str>) {
    let height = ICON + if headline.is_some() { 34.0 } else { 0.0 };
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), height), Sense::hover());
    let icon = Rect::from_min_size(rect.left_top(), egui::vec2(ICON, ICON));
    egui::Image::new(egui::include_image!("../assets/icon/kuulo.svg")).paint_at(ui, icon);
    let letters = egui::Image::new(egui::include_image!("../assets/wordmark/letters.svg")).tint(theme.accent);
    let wave = egui::Image::new(egui::include_image!("../assets/wordmark/wave.svg")).tint(theme.accent2);
    let size = letters.load_and_calc_size(ui, egui::vec2(f32::INFINITY, 38.0)).unwrap_or(egui::vec2(120.0, 38.0));
    let word = Rect::from_min_size(egui::pos2(icon.right() + 18.0, icon.center().y - size.y / 2.0), size);
    letters.paint_at(ui, word);
    wave.paint_at(ui, word);
    let painter = ui.painter();
    painter.text(egui::pos2(word.right() + 10.0, word.bottom() + 3.0), Align2::LEFT_BOTTOM, format!("v{}", kuulo_core::VERSION), FontId::proportional(20.0), theme.muted);
    if let Some(headline) = headline {
        painter.text(egui::pos2(rect.left(), icon.bottom() + 12.0), Align2::LEFT_TOP, headline, FontId::new(16.0, FontFamily::Name(theme::SEMIBOLD.into())), theme.text);
    }
}

/// The logo's square wave as a divider, copper to verdigris, drawn as far as `trace` (0 to
/// 1) with a pen at its tip while it draws.
fn divider(ui: &mut egui::Ui, theme: &Theme, trace: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 30.0), Sense::hover());
    let (period, amplitude, roundness) = (48.0, 6.5, 2.2_f32);
    let end = rect.left() + rect.width() * trace;
    let points: Vec<Pos2> = (0..)
        .map(|i| rect.left() + i as f32)
        .take_while(|&x| x <= end)
        .map(|x| {
            let s = (roundness * (TAU * (x - rect.left()) / period).sin()).tanh() / roundness.tanh();
            egui::pos2(x, rect.center().y - amplitude * s)
        })
        .collect();
    let (copper, verdigris) = (theme.accent, theme.accent2);
    let shade = move |p: Pos2| copper.lerp_to_gamma(verdigris, ((p.x - rect.left()) / rect.width()).clamp(0.0, 1.0));
    let painter = ui.painter();
    if let Some(&tip) = points.last() {
        painter.add(PathShape::line(points, PathStroke::new_uv(3.0, move |_, p| shade(p))));
        if trace < 1.0 {
            painter.circle_filled(tip, 8.0, shade(tip).gamma_multiply(0.25));
            painter.circle_filled(tip, 3.5, shade(tip));
        }
    }
}

/// The releases' lines; each version is named above its own when there is more than one.
fn releases(ui: &mut egui::Ui, theme: &Theme, notes: &[Notes]) {
    for (i, n) in notes.iter().enumerate() {
        if notes.len() > 1 {
            ui.add_space(if i == 0 { 4.0 } else { 14.0 });
            egui::Frame::new().fill(theme.bg_deep).corner_radius(CornerRadius::same(6)).inner_margin(Margin::symmetric(8, 2)).show(ui, |ui| {
                ui.label(RichText::new(&n.version).size(12.0).color(theme.muted));
            });
        }
        for l in &n.lines {
            line(ui, theme, l);
        }
    }
}

/// One changelog line: a list item gets a marker, copper for something new and verdigris for
/// a fix, and `**bold**` and `` `code` `` are drawn as such.
fn line(ui: &mut egui::Ui, theme: &Theme, text: &str) {
    let item = text.strip_prefix("- ");
    let text = item.unwrap_or(text);
    let size = 13.5;
    let mut job = LayoutJob::default();
    let (mut bold, mut code, mut start, mut i) = (false, false, 0, 0);
    let append = |job: &mut LayoutJob, s: &str, bold: bool, code: bool| {
        let family = if code {
            FontFamily::Monospace
        } else if bold {
            FontFamily::Name(theme::SEMIBOLD.into())
        } else {
            FontFamily::Proportional
        };
        job.append(s, 0.0, TextFormat { font_id: FontId::new(if code { size - 1.0 } else { size }, family), color: theme.text, ..Default::default() });
    };
    while i < text.len() {
        if !code && text[i..].starts_with("**") {
            append(&mut job, &text[start..i], bold, code);
            bold = !bold;
            i += 2;
            start = i;
        } else if text[i..].starts_with('`') {
            append(&mut job, &text[start..i], bold, code);
            code = !code;
            i += 1;
            start = i;
        } else {
            i += text[i..].chars().next().map_or(1, char::len_utf8);
        }
    }
    append(&mut job, &text[start..], bold, code);
    ui.add_space(6.0);
    ui.horizontal_top(|ui| {
        if item.is_some() {
            let marker = if text.starts_with("**Fixed:**") { theme.accent2 } else { theme.accent };
            let (r, _) = ui.allocate_exact_size(egui::vec2(10.0, size * 1.25), Sense::hover());
            ui.painter().circle_filled(egui::pos2(r.left() + 3.0, r.center().y), 3.0, marker);
        }
        ui.add(egui::Label::new(job).wrap());
    });
}

impl App {
    pub fn ui_whats_new(&mut self, ctx: &egui::Context) {
        let Some(card) = &mut self.whats_new else {
            return;
        };
        let theme = self.theme.clone();
        let now = ctx.input(|i| i.time);
        let t = ((now - *card.shown_at.get_or_insert(now) - TRACE_DELAY_SECS) / TRACE_SECS).clamp(0.0, 1.0) as f32;
        if t < 1.0 {
            ctx.request_repaint();
        }
        let trace = if t < 0.5 { 4.0 * t.powi(3) } else { 1.0 - (2.0 - 2.0 * t).powi(3) / 2.0 };
        let width = CARD_W.min(ctx.content_rect().width() - 48.0);
        let mut close = false;
        let frame = egui::Frame::new().fill(theme.surface).stroke(Stroke::new(1.0, theme.line)).corner_radius(CornerRadius::same(14)).inner_margin(Margin::same(26));
        let modal = egui::Modal::new(egui::Id::new("whats-new")).frame(frame).backdrop_color(Color32::from_black_alpha(150)).show(ctx, |ui| {
            ui.set_width(width);
            lockup(ui, &theme, card.headline.as_deref());
            ui.add_space(10.0);
            divider(ui, &theme, trace);
            ui.add_space(10.0);
            ui.label(theme::semibold("What's new").size(15.0));
            egui::ScrollArea::vertical().max_height(ctx.content_rect().height() * 0.45).show(ui, |ui| releases(ui, &theme, &card.releases));
            ui.add_space(22.0);
            ui.horizontal(|ui| {
                let url = format!("{}/releases/tag/v{}", kuulo_core::update::REPOSITORY, kuulo_core::VERSION);
                if ui.add(egui::Button::new(theme::icon_text(icons::ARROW_SQUARE_OUT, "Release page", 14.0, FontFamily::Proportional, Color32::PLACEHOLDER)).min_size(egui::vec2(0.0, 32.0))).on_hover_text(&url).clicked() {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(url));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let label = RichText::new("Continue").size(14.0).family(FontFamily::Name(theme::SEMIBOLD.into())).color(theme.on_accent());
                    close = ui.add(egui::Button::new(label).fill(theme.accent).corner_radius(CornerRadius::same(6)).min_size(egui::vec2(120.0, 32.0))).clicked();
                });
            });
        });
        if close || modal.should_close() || ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
            self.whats_new = None;
        }
    }
}

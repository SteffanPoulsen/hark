//! The controls that edit one export preset, shared by the export card and the settings
//! presets page. Each returns whether it changed the preset.

use egui::RichText;
use hark_core::config::{Normalize, OutputFormat};
use hark_core::pcm::{DownmixMode, FadeShape};
use hark_core::ExportPreset;

pub const CONTROL_H: f32 = 26.0;
const RATES: [u32; 5] = [22_050, 44_100, 48_000, 96_000, 192_000];
const DOWNMIX: [DownmixMode; 4] = [DownmixMode::Average, DownmixMode::SumMinus3, DownmixMode::Left, DownmixMode::Right];
const SHAPES: [FadeShape; 3] = [FadeShape::EqualPower, FadeShape::Linear, FadeShape::Logarithmic];

pub fn rate_label(rate: Option<u32>) -> String {
    rate.map(|r| format!("{} kHz", trim_zeros(r as f64 / 1000.0))).unwrap_or_else(|| "Source rate".into())
}

fn trim_zeros(v: f64) -> String {
    let s = format!("{v:.3}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn combo(id: (&str, &str), width: f32, text: String) -> egui::ComboBox {
    egui::ComboBox::from_id_salt(id).width(width).truncate().selected_text(RichText::new(text).size(13.0))
}

fn pick<T: PartialEq + Copy>(ui: &mut egui::Ui, current: &mut T, value: T, label: &str) -> bool {
    if ui.selectable_label(*current == value, label).clicked() && *current != value {
        *current = value;
        return true;
    }
    false
}

fn number(ui: &mut egui::Ui, v: &mut f64, range: std::ops::RangeInclusive<f64>, suffix: &str, width: f32) -> bool {
    ui.add_sized(egui::vec2(width, CONTROL_H), egui::DragValue::new(v).speed(0.1).range(range).fixed_decimals(1).suffix(suffix)).changed()
}

pub fn format(ui: &mut egui::Ui, id: &str, p: &mut ExportPreset) -> bool {
    let mut changed = false;
    combo((id, "format"), 66.0, p.format.extension().to_uppercase()).show_ui(ui, |ui| {
        changed |= pick(ui, &mut p.format, OutputFormat::Wav, "WAV");
        changed |= pick(ui, &mut p.format, OutputFormat::Flac, "FLAC");
    });
    if changed && p.format == OutputFormat::Flac && p.bits > 24 {
        p.bits = 24;
    }
    changed
}

pub fn bits(ui: &mut egui::Ui, id: &str, p: &mut ExportPreset) -> bool {
    let mut changed = false;
    combo((id, "bits"), 78.0, format!("{} bit", p.bits)).show_ui(ui, |ui| {
        changed |= pick(ui, &mut p.bits, 16, "16 bit");
        changed |= pick(ui, &mut p.bits, 24, "24 bit");
        if p.format == OutputFormat::Wav {
            changed |= pick(ui, &mut p.bits, 32, "32 bit float");
        }
    });
    changed
}

pub fn sample_rate(ui: &mut egui::Ui, id: &str, p: &mut ExportPreset) -> bool {
    let mut changed = false;
    combo((id, "rate"), 110.0, rate_label(p.sample_rate)).show_ui(ui, |ui| {
        changed |= pick(ui, &mut p.sample_rate, None, "Source rate");
        for r in RATES {
            changed |= pick(ui, &mut p.sample_rate, Some(r), &rate_label(Some(r)));
        }
    });
    changed
}

pub fn channels(ui: &mut egui::Ui, id: &str, p: &mut ExportPreset) -> bool {
    let mut changed = false;
    let label = p.mono.map(|m| m.name().to_string()).unwrap_or_else(|| "Source channels".into());
    combo((id, "channels"), 140.0, label).show_ui(ui, |ui| {
        changed |= pick(ui, &mut p.mono, None, "Source channels");
        for m in DOWNMIX {
            changed |= pick(ui, &mut p.mono, Some(m), m.name());
        }
    });
    changed
}

pub fn dither(ui: &mut egui::Ui, p: &mut ExportPreset) -> bool {
    ui.add_enabled(p.bits < 32, crate::theme::Check::new(&mut p.dither, RichText::new("Dither").size(13.0))).changed()
}

/// Mode, then the target and, for LUFS, the true-peak ceiling.
pub fn normalize(ui: &mut egui::Ui, id: &str, p: &mut ExportPreset) -> bool {
    let mut changed = false;
    let mode = match p.normalize {
        Normalize::None => 0,
        Normalize::Peak { .. } => 1,
        Normalize::Lufs { .. } => 2,
    };
    let mut want = mode;
    combo((id, "normalize"), 72.0, ["None", "Peak", "LUFS"][mode].to_string()).show_ui(ui, |ui| {
        pick(ui, &mut want, 0, "None");
        pick(ui, &mut want, 1, "Peak");
        pick(ui, &mut want, 2, "LUFS");
    });
    if want != mode {
        p.normalize = match want {
            1 => Normalize::Peak { target_dbfs: -1.0 },
            2 => Normalize::Lufs { target_lufs: -16.0, true_peak_ceiling_dbfs: -1.0 },
            _ => Normalize::None,
        };
        changed = true;
    }
    match &mut p.normalize {
        Normalize::None => {}
        Normalize::Peak { target_dbfs } => changed |= number(ui, target_dbfs, -40.0..=0.0, " dBFS", 72.0),
        Normalize::Lufs { target_lufs, true_peak_ceiling_dbfs } => {
            changed |= number(ui, target_lufs, -40.0..=0.0, " LU", 66.0);
            changed |= number(ui, true_peak_ceiling_dbfs, -12.0..=0.0, " dBTP", 72.0);
        }
    }
    changed
}

pub fn fade_shape(ui: &mut egui::Ui, id: &str, p: &mut ExportPreset) -> bool {
    let mut changed = false;
    combo((id, "fade"), 120.0, p.fade_shape.name().to_string()).show_ui(ui, |ui| {
        for s in SHAPES {
            changed |= pick(ui, &mut p.fade_shape, s, s.name());
        }
    });
    changed
}

pub const TEMPLATE_HELP: &str = "The file name inside the export folder; the extension follows the format.\n{name} source stem\n{root} library root name\n{folder} source folder name\n{relpath} source folder under its root\n{date} YYYY-MM-DD";

pub fn path_template(ui: &mut egui::Ui, p: &mut ExportPreset, width: f32) -> bool {
    ui.add_sized(egui::vec2(width, CONTROL_H), egui::TextEdit::singleline(&mut p.path_template).font(egui::TextStyle::Monospace)).on_hover_text(TEMPLATE_HELP).changed()
}

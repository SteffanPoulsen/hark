//! The bottom waveform: a filled envelope with an RMS band at coarse zoom, the sample curve at
//! fine zoom, a draggable selection, fade handles in their own lane above the wave so nothing
//! else competes for the grab, and the playhead. Wheel zooms, middle drag and shift-wheel pan.

use egui::epaint::{Mesh, Vertex, WHITE_UV};
use egui::{Align2, Color32, CursorIcon, FontId, PointerButton, Pos2, Rect, Sense, Shape, Stroke, StrokeKind, Ui, Vec2};
use hark_core::pcm::FadeShape;
use hark_core::{Pcm, Peaks};

const LANE_H: f32 = 20.0;
const HANDLE_W: f32 = 14.0;
const HANDLE_GRAB_PX: f32 = 16.0;
const EDGE_GRAB_PX: f32 = 8.0;
/// Below this many frames per pixel the real samples are drawn instead of the envelope.
const SAMPLE_VIEW_FRAMES_PER_PX: f64 = 3.0;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Drag {
    New { anchor: f64 },
    Start,
    End,
    Move { grab: f64 },
    FadeIn,
    FadeOut,
}

#[derive(Debug, Default)]
pub struct WaveState {
    /// Visible span in seconds.
    pub view: (f64, f64),
    pub selection: Option<(f64, f64)>,
    pub fade_in: f64,
    pub fade_out: f64,
    pub fade_shape: FadeShape,
    drag: Option<Drag>,
    /// The edge moved last. When both edges sit on one spot, it is the one a grab takes, so
    /// dragging back reopens the selection the way it was just closed.
    last_edge: Option<Drag>,
}

pub enum WaveEvent {
    None,
    Seek(f64),
}

pub struct WaveColors {
    pub peak: Color32,
    pub rms: Color32,
    pub selection: Color32,
    pub fade: Color32,
    pub playhead: Color32,
    pub axis: Color32,
}

impl WaveState {
    pub fn reset(&mut self, duration: f64) {
        self.view = (0.0, duration.max(1e-3));
        self.selection = None;
        self.fade_in = 0.0;
        self.fade_out = 0.0;
        self.drag = None;
        self.last_edge = None;
    }

    /// The span the fades hang off: the selection, or the whole file.
    pub fn bounds(&self, duration: f64) -> (f64, f64) {
        self.selection.unwrap_or((0.0, duration))
    }

    pub fn clamp_fades(&mut self, duration: f64) {
        let (a, b) = self.bounds(duration);
        let len = (b - a).max(0.0);
        self.fade_in = self.fade_in.clamp(0.0, len);
        self.fade_out = self.fade_out.clamp(0.0, len - self.fade_in);
    }

    pub fn zoom_to_selection(&mut self) {
        if let Some((a, b)) = self.selection {
            if b > a {
                let pad = (b - a) * 0.05;
                self.view = (a - pad, b + pad);
            }
        }
    }

    fn zoom_at(&mut self, anchor: f64, factor: f64, duration: f64) {
        let (v0, v1) = self.view;
        let span = v1 - v0;
        let new_span = (span / factor).clamp(0.0005, duration);
        let frac = (anchor - v0) / span;
        let a = (anchor - frac * new_span).clamp(0.0, duration - new_span);
        self.view = (a, a + new_span);
    }

    fn pan(&mut self, dt: f64, duration: f64) {
        let (v0, v1) = self.view;
        let span = v1 - v0;
        let a = (v0 + dt).clamp(0.0, duration - span);
        self.view = (a, a + span);
    }
}

/// What the pointer would grab at `p`, before any drag starts.
fn hit(state: &WaveState, lane: Rect, p: Pos2, duration: f64, x_of: &dyn Fn(f64) -> f32) -> Option<Drag> {
    let (a, b) = state.bounds(duration);
    if lane.contains(p) {
        let din = (p.x - x_of(a + state.fade_in)).abs();
        let dout = (p.x - x_of(b - state.fade_out)).abs();
        return if din <= HANDLE_GRAB_PX && din <= dout {
            Some(Drag::FadeIn)
        } else if dout <= HANDLE_GRAB_PX {
            Some(Drag::FadeOut)
        } else {
            None
        };
    }
    state.selection?;
    let near_a = (p.x - x_of(a)).abs() <= EDGE_GRAB_PX;
    let near_b = (p.x - x_of(b)).abs() <= EDGE_GRAB_PX;
    match (near_a, near_b) {
        (true, true) => Some(state.last_edge.unwrap_or(Drag::Start)),
        (true, false) => Some(Drag::Start),
        (false, true) => Some(Drag::End),
        (false, false) => None,
    }
}

/// A filled band between two polylines sharing x positions, as one mesh.
fn band(xs: &[f32], top: &[f32], bottom: &[f32], color: Color32) -> Shape {
    let mut mesh = Mesh::default();
    for i in 0..xs.len() {
        mesh.vertices.push(Vertex { pos: Pos2::new(xs[i], top[i]), uv: WHITE_UV, color });
        mesh.vertices.push(Vertex { pos: Pos2::new(xs[i], bottom[i]), uv: WHITE_UV, color });
        if i > 0 {
            let b = (i * 2) as u32;
            mesh.add_triangle(b - 2, b - 1, b);
            mesh.add_triangle(b - 1, b + 1, b);
        }
    }
    Shape::mesh(mesh)
}

fn draw_envelope(painter: &egui::Painter, peaks: &Peaks, rect: Rect, v0: f64, v1: f64, colors: &WaveColors) {
    let channels = peaks.channels.max(1) as usize;
    let lane_h = rect.height() / channels as f32;
    let cols = rect.width().max(1.0) as usize;
    for ch in 0..channels {
        let mid = rect.top() + lane_h * (ch as f32 + 0.5);
        let amp = lane_h * 0.46;
        painter.hline(rect.x_range(), mid, Stroke::new(1.0, colors.axis));
        let bins = peaks.columns(ch, v0, v1, cols);
        let xs: Vec<f32> = (0..cols).map(|i| rect.left() + i as f32 + 0.5).collect();
        let top: Vec<f32> = bins.iter().map(|b| mid - b.max.clamp(-1.0, 1.0) * amp).collect();
        let bottom: Vec<f32> = bins.iter().map(|b| mid - b.min.clamp(-1.0, 1.0) * amp).collect();
        painter.add(band(&xs, &top, &bottom, colors.peak));
        let rms_top: Vec<f32> = bins.iter().map(|b| mid - b.rms.clamp(0.0, 1.0) * amp).collect();
        let rms_bottom: Vec<f32> = bins.iter().map(|b| mid + b.rms.clamp(0.0, 1.0) * amp).collect();
        painter.add(band(&xs, &rms_top, &rms_bottom, colors.rms));
        let outline: Vec<Pos2> = xs.iter().zip(&top).map(|(&x, &y)| Pos2::new(x, y)).collect();
        painter.add(Shape::line(outline, Stroke::new(1.0, colors.peak.gamma_multiply(1.3))));
        let outline: Vec<Pos2> = xs.iter().zip(&bottom).map(|(&x, &y)| Pos2::new(x, y)).collect();
        painter.add(Shape::line(outline, Stroke::new(1.0, colors.peak.gamma_multiply(1.3))));
    }
}

fn draw_samples(painter: &egui::Painter, pcm: &Pcm, rect: Rect, v0: f64, v1: f64, colors: &WaveColors) {
    let channels = pcm.channels.max(1) as usize;
    let lane_h = rect.height() / channels as f32;
    let rate = pcm.sample_rate as f64;
    let frames = pcm.frames();
    let f0 = ((v0 * rate).floor().max(0.0) as usize).min(frames);
    let f1 = ((v1 * rate).ceil().max(0.0) as usize + 1).min(frames);
    let span = v1 - v0;
    let x_of = |f: usize| rect.left() + ((f as f64 / rate - v0) / span) as f32 * rect.width();
    let frames_per_px = (f1 - f0) as f64 / rect.width().max(1.0) as f64;
    for ch in 0..channels {
        let mid = rect.top() + lane_h * (ch as f32 + 0.5);
        let amp = lane_h * 0.46;
        painter.hline(rect.x_range(), mid, Stroke::new(1.0, colors.axis));
        let pts: Vec<Pos2> = (f0..f1).map(|f| Pos2::new(x_of(f), mid - pcm.samples[f * channels + ch].clamp(-1.0, 1.0) * amp)).collect();
        if pts.len() >= 2 {
            let xs: Vec<f32> = pts.iter().map(|p| p.x).collect();
            let top: Vec<f32> = pts.iter().map(|p| p.y.min(mid)).collect();
            let bottom: Vec<f32> = pts.iter().map(|p| p.y.max(mid)).collect();
            painter.add(band(&xs, &top, &bottom, colors.rms.gamma_multiply(0.6)));
            painter.add(Shape::line(pts.clone(), Stroke::new(1.5, colors.peak.gamma_multiply(1.4))));
        }
        if frames_per_px < 0.25 {
            for p in &pts {
                painter.circle_filled(*p, 2.0, colors.peak.gamma_multiply(1.6));
            }
        }
    }
}

pub fn show(ui: &mut Ui, state: &mut WaveState, peaks: Option<&Peaks>, samples: Option<&Pcm>, duration: f64, playhead: Option<f64>, height: f32, colors: &WaveColors) -> WaveEvent {
    let width = ui.available_width();
    let (resp, painter) = ui.allocate_painter(Vec2::new(width, height), Sense::click_and_drag());
    let full = resp.rect;
    let lane = Rect::from_min_max(full.left_top(), Pos2::new(full.right(), full.top() + LANE_H));
    let rect = Rect::from_min_max(Pos2::new(full.left(), lane.bottom()), full.right_bottom());
    let visuals = ui.visuals().clone();
    painter.rect_filled(full, 4.0, visuals.extreme_bg_color);
    painter.rect_filled(lane, 0.0, visuals.faint_bg_color);

    let Some(peaks) = peaks else {
        painter.text(rect.center(), Align2::CENTER_CENTER, "no file", FontId::proportional(14.0), visuals.weak_text_color());
        return WaveEvent::None;
    };
    let duration = duration.max(1e-6);
    if state.view.1 <= state.view.0 {
        state.view = (0.0, duration);
    }

    // Wheel: zoom around the pointer; shift-wheel (which egui turns horizontal) and middle drag pan.
    if resp.hovered() {
        let (zoom, scroll) = ui.input(|i| (i.zoom_delta(), i.smooth_scroll_delta));
        let (v0, v1) = state.view;
        let span = v1 - v0;
        if let Some(p) = resp.hover_pos() {
            let anchor = v0 + ((p.x - rect.left()) / rect.width()) as f64 * span;
            if zoom != 1.0 {
                state.zoom_at(anchor, zoom as f64, duration);
            } else if scroll.y != 0.0 {
                state.zoom_at(anchor, (scroll.y as f64 * 0.004).exp(), duration);
            }
        }
        if scroll.x != 0.0 {
            state.pan(-(scroll.x / rect.width()) as f64 * span, duration);
        }
    }
    if resp.dragged_by(PointerButton::Middle) {
        let (v0, v1) = state.view;
        let dx = resp.drag_delta().x;
        state.pan(-(dx / rect.width()) as f64 * (v1 - v0), duration);
        ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
    }
    let (v0, v1) = state.view;
    let span = v1 - v0;
    let x_of = |t: f64| rect.left() + ((t - v0) / span) as f32 * rect.width();
    let t_of = |x: f32| v0 + ((x - rect.left()) / rect.width()) as f64 * span;

    // The wave itself.
    let frames_per_px = span * peaks.sample_rate as f64 / rect.width().max(1.0) as f64;
    match samples {
        Some(pcm) if frames_per_px * (pcm.sample_rate as f64 / peaks.sample_rate as f64) <= SAMPLE_VIEW_FRAMES_PER_PX => draw_samples(&painter, pcm, rect, v0, v1, colors),
        _ => draw_envelope(&painter, peaks, rect, v0, v1, colors),
    }

    // Selection, fades, handles.
    let hovered_hit = resp.hover_pos().and_then(|p| hit(state, lane, p, duration, &x_of));
    let active = state.drag.or(hovered_hit);
    if let Some((a, b)) = state.selection {
        let xa = x_of(a).clamp(rect.left(), rect.right());
        let xb = x_of(b).clamp(rect.left(), rect.right());
        let sel = Rect::from_min_max(Pos2::new(xa, rect.top()), Pos2::new(xb, rect.bottom()));
        painter.rect_filled(sel, 0.0, colors.selection.gamma_multiply(0.18));
        let edge = |which: Drag| if active == Some(which) { Stroke::new(2.5, Color32::WHITE) } else { Stroke::new(1.0, colors.selection) };
        painter.vline(xa, rect.y_range(), edge(Drag::Start));
        painter.vline(xb, rect.y_range(), edge(Drag::End));
    }
    let (a, b) = state.bounds(duration);
    let curve = |from: f64, to: f64, rising: bool| {
        let n = 32;
        let pts: Vec<Pos2> = (0..=n)
            .map(|i| {
                let t = i as f32 / n as f32;
                let g = state.fade_shape.gain(if rising { t } else { 1.0 - t });
                Pos2::new(x_of(from + (to - from) * t as f64), rect.bottom() - g * rect.height())
            })
            .collect();
        Shape::line(pts, Stroke::new(1.5, colors.fade))
    };
    if state.fade_in > 0.0 {
        painter.add(curve(a, a + state.fade_in, true));
    }
    if state.fade_out > 0.0 {
        painter.add(curve(b - state.fade_out, b, false));
    }
    let handle = |x: f32, pointing_right: bool, hot: bool| {
        let dir = if pointing_right { 1.0 } else { -1.0 };
        let top = lane.top() + 3.0;
        let bottom = lane.bottom() - 3.0;
        let fill = if hot { Color32::WHITE } else { colors.fade };
        vec![
            Shape::convex_polygon(vec![Pos2::new(x, top), Pos2::new(x + dir * HANDLE_W, top), Pos2::new(x, bottom)], fill, Stroke::NONE),
            Shape::Vec(Shape::dashed_line(&[Pos2::new(x, lane.bottom()), Pos2::new(x, rect.bottom())], Stroke::new(1.0, fill.gamma_multiply(0.6)), 4.0, 4.0)),
        ]
    };
    for s in handle(x_of(a + state.fade_in), true, active == Some(Drag::FadeIn)) {
        painter.add(s);
    }
    for s in handle(x_of(b - state.fade_out), false, active == Some(Drag::FadeOut)) {
        painter.add(s);
    }
    painter.text(lane.left_center() + Vec2::new(6.0, 0.0), Align2::LEFT_CENTER, "Fades", FontId::proportional(10.0), visuals.weak_text_color());

    // Playhead.
    if let Some(t) = playhead {
        if t >= v0 && t <= v1 {
            painter.vline(x_of(t), rect.y_range(), Stroke::new(1.5, colors.playhead));
        }
    }

    // Time ruler.
    let font = FontId::monospace(10.0);
    painter.text(rect.left_top() + Vec2::new(4.0, 2.0), Align2::LEFT_TOP, fmt_time(v0), font.clone(), visuals.weak_text_color());
    painter.text(rect.right_top() + Vec2::new(-4.0, 2.0), Align2::RIGHT_TOP, fmt_time(v1), font.clone(), visuals.weak_text_color());
    if let Some((a, b)) = state.selection {
        painter.text(
            rect.center_bottom() + Vec2::new(0.0, -2.0),
            Align2::CENTER_BOTTOM,
            format!("{} – {}  ({})", fmt_time(a), fmt_time(b), fmt_time(b - a)),
            font,
            colors.fade,
        );
    }
    painter.rect_stroke(full, 4.0, Stroke::new(1.0, visuals.widgets.noninteractive.bg_stroke.color), StrokeKind::Inside);

    // Cursor.
    match active {
        Some(Drag::Start) | Some(Drag::End) | Some(Drag::FadeIn) | Some(Drag::FadeOut) => ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal),
        Some(Drag::Move { .. }) => ui.ctx().set_cursor_icon(CursorIcon::Grabbing),
        _ if resp.hovered() && lane.contains(resp.hover_pos().unwrap_or(Pos2::ZERO)) => {}
        _ if resp.hovered() => ui.ctx().set_cursor_icon(CursorIcon::Text),
        _ => {}
    }

    // Left-button interaction.
    let mut event = WaveEvent::None;
    if resp.drag_started_by(PointerButton::Primary) {
        // The drag is reported a few pixels after the press; a fast flick would otherwise miss
        // the handle it started on.
        let origin = ui.input(|i| i.pointer.press_origin()).or_else(|| resp.interact_pointer_pos());
        if let Some(p) = origin {
            let t = t_of(p.x).clamp(0.0, duration);
            state.drag = match hit(state, lane, p, duration, &x_of) {
                Some(d) => Some(d),
                None if lane.contains(p) => None,
                None => match state.selection {
                    Some((a, b)) if t > a && t < b && ui.input(|i| i.modifiers.shift) => Some(Drag::Move { grab: t - a }),
                    _ => Some(Drag::New { anchor: t }),
                },
            };
        }
    }
    if resp.dragged_by(PointerButton::Primary) {
        if let (Some(drag), Some(p)) = (state.drag, resp.interact_pointer_pos()) {
            let t = t_of(p.x).clamp(0.0, duration);
            match drag {
                Drag::New { anchor } => {
                    let (a, b) = if t < anchor { (t, anchor) } else { (anchor, t) };
                    state.selection = Some((a, b));
                    state.last_edge = Some(if t < anchor { Drag::Start } else { Drag::End });
                }
                Drag::Start => {
                    if let Some((_, b)) = state.selection {
                        state.selection = Some((t.min(b), b));
                        state.last_edge = Some(Drag::Start);
                    }
                }
                Drag::End => {
                    if let Some((a, _)) = state.selection {
                        state.selection = Some((a, t.max(a)));
                        state.last_edge = Some(Drag::End);
                    }
                }
                Drag::Move { grab } => {
                    if let Some((a, b)) = state.selection {
                        let len = b - a;
                        let na = (t - grab).clamp(0.0, duration - len);
                        state.selection = Some((na, na + len));
                    }
                }
                Drag::FadeIn => state.fade_in = (t - state.bounds(duration).0).max(0.0),
                Drag::FadeOut => state.fade_out = (state.bounds(duration).1 - t).max(0.0),
            }
            state.clamp_fades(duration);
        }
    }
    if resp.drag_stopped_by(PointerButton::Primary) {
        // A fresh drag that never opened up was a sloppy click. An existing selection whose
        // edges were dragged together stays, zero-length, with the start handle first in line
        // for the next grab.
        if let (Some(Drag::New { .. }), Some((a, b))) = (state.drag, state.selection) {
            if b - a < 1e-4 {
                state.selection = None;
            }
        }
        state.drag = None;
    }
    if resp.clicked_by(PointerButton::Primary) {
        if let Some(p) = resp.interact_pointer_pos() {
            if !lane.contains(p) {
                event = WaveEvent::Seek(t_of(p.x).clamp(0.0, duration));
            }
        }
    }
    if resp.double_clicked_by(PointerButton::Primary) {
        state.view = (0.0, duration);
    }
    event
}

pub fn fmt_time(s: f64) -> String {
    let s = s.max(0.0);
    if s >= 60.0 {
        format!("{}:{:06.3}", (s / 60.0).floor() as u64, s % 60.0)
    } else {
        format!("{s:.3}")
    }
}

//! The bottom waveform: a filled envelope with an RMS band, reduced per pixel column from the
//! peaks or, once a column covers less than a bin, from the samples; the sample curve at the
//! finest zoom; a draggable selection, fade handles in their own lane above the wave so nothing
//! else competes for the grab, and the playhead. Wheel zooms, middle drag and shift-wheel pan.

use egui::epaint::{Mesh, Vertex, WHITE_UV};
use egui::{Align2, Color32, CursorIcon, FontId, PointerButton, Pos2, Rect, Sense, Shape, Stroke, Ui, Vec2};
use hark_core::pcm::FadeShape;
use hark_core::peaks::sample_columns;
use hark_core::{Pcm, Peaks};

/// Height of the fade lane above the wave.
pub const LANE_H: f32 = 20.0;
const HANDLE_W: f32 = 14.0;
const HANDLE_GRAB_PX: f32 = 16.0;
const EDGE_GRAB_PX: f32 = 8.0;
/// How long a flash lasts, and how much of it is left at `t` of that: bright almost to the
/// end, then off. The export button follows the same curve.
pub const FLASH_SECS: f32 = 0.3;
pub fn flash_left(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - t * t * t
}
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

#[derive(Debug)]
pub struct WaveState {
    /// Visible span in seconds.
    pub view: (f64, f64),
    pub selection: Option<(f64, f64)>,
    pub fade_in: f64,
    pub fade_out: f64,
    pub fade_shape: FadeShape,
    /// Linear gain the drawing is scaled by, so the picture follows the playback volume.
    pub gain: f32,
    drag: Option<Drag>,
    /// A span lit up for a moment, from when: the part just exported.
    flash: Option<((f64, f64), std::time::Instant)>,
    /// The edge moved last. When both edges sit on one spot, it is the one a grab takes, so
    /// dragging back reopens the selection the way it was just closed.
    last_edge: Option<Drag>,
    /// The fade set last. It keeps its length when the two overlap, pushing the other back,
    /// and takes the grab when both handles sit on one spot.
    last_fade: Option<Fade>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Fade {
    In,
    Out,
}

pub enum WaveEvent {
    None,
    Seek(f64),
    /// A Ctrl-drag began on the wave: the selection is being dragged out of the window.
    DragOut,
}

pub struct WaveColors {
    pub peak: Color32,
    pub rms: Color32,
    pub selection: Color32,
    pub fade: Color32,
    pub playhead: Color32,
    pub axis: Color32,
    pub flash: Color32,
}

impl Default for WaveState {
    fn default() -> Self {
        WaveState { view: (0.0, 0.0), selection: None, fade_in: 0.0, fade_out: 0.0, fade_shape: FadeShape::default(), gain: 1.0, drag: None, flash: None, last_edge: None, last_fade: None }
    }
}

impl WaveState {
    pub fn reset(&mut self, duration: f64) {
        self.view = (0.0, duration.max(1e-3));
        self.selection = None;
        self.fade_in = 0.0;
        self.fade_out = 0.0;
        self.drag = None;
        self.last_edge = None;
        self.last_fade = None;
    }

    /// The span the fades hang off: the selection, or the whole file.
    pub fn bounds(&self, duration: f64) -> (f64, f64) {
        self.selection.unwrap_or((0.0, duration))
    }

    /// Lights the span up for `FLASH_SECS`.
    pub fn flash(&mut self, span: (f64, f64)) {
        self.flash = Some((span, std::time::Instant::now()));
    }

    /// Marks a fade as the one set last, so clamping keeps it and shortens the other.
    pub fn touch_fade(&mut self, which: Fade) {
        self.last_fade = Some(which);
    }

    pub fn clamp_fades(&mut self, duration: f64) {
        let (a, b) = self.bounds(duration);
        let len = (b - a).max(0.0);
        if self.last_fade == Some(Fade::Out) {
            self.fade_out = self.fade_out.clamp(0.0, len);
            self.fade_in = self.fade_in.clamp(0.0, len - self.fade_out);
        } else {
            self.fade_in = self.fade_in.clamp(0.0, len);
            self.fade_out = self.fade_out.clamp(0.0, len - self.fade_in);
        }
    }

    /// Pans the view, keeping its width, until the span is inside it; a span wider than the
    /// view becomes the view.
    pub fn reveal(&mut self, span: (f64, f64), duration: f64) {
        let (v0, v1) = self.view;
        let w = v1 - v0;
        if span.0 >= v0 && span.1 <= v1 {
            return;
        }
        if span.1 - span.0 >= w {
            self.fit(span, duration);
            return;
        }
        let start = (span.0 - w * 0.1).min(duration - w).max(0.0);
        self.view = (start, start + w);
    }

    pub fn zoom_to_selection(&mut self, duration: f64) {
        if let Some((a, b)) = self.selection {
            if b > a {
                self.fit((a, b), duration);
            }
        }
    }

    /// The view becomes the span with a little room on each side, kept inside the file.
    fn fit(&mut self, span: (f64, f64), duration: f64) {
        let pad = (span.1 - span.0) * 0.05;
        let w = (span.1 - span.0 + 2.0 * pad).min(duration);
        let start = (span.0 - pad).clamp(0.0, duration - w);
        self.view = (start, start + w);
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
        let last = if state.last_fade == Some(Fade::Out) { Drag::FadeOut } else { Drag::FadeIn };
        return match (din <= HANDLE_GRAB_PX, dout <= HANDLE_GRAB_PX) {
            (true, true) if (din - dout).abs() < 1.0 => Some(last),
            (true, true) => Some(if din < dout { Drag::FadeIn } else { Drag::FadeOut }),
            (true, false) => Some(Drag::FadeIn),
            (false, true) => Some(Drag::FadeOut),
            (false, false) => None,
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

/// A fade handle: a triangle in the lane pointing into the fade, and a dashed drop line down
/// the wave.
fn handle(lane: Rect, rect: Rect, x: f32, pointing_right: bool, fill: Color32) -> Vec<Shape> {
    let dir = if pointing_right { 1.0 } else { -1.0 };
    let top = lane.top() + 3.0;
    let bottom = lane.bottom() - 3.0;
    vec![
        Shape::convex_polygon(vec![Pos2::new(x, top), Pos2::new(x + dir * HANDLE_W, top), Pos2::new(x, bottom)], fill, Stroke::NONE),
        Shape::Vec(Shape::dashed_line(&[Pos2::new(x, lane.bottom()), Pos2::new(x, rect.bottom())], Stroke::new(1.0, fill.gamma_multiply(0.6)), 4.0, 4.0)),
    ]
}

/// The tick spacing for a span: the first of a 1-2-5 sequence that keeps labels apart.
fn ruler_step(span: f64, width: f32) -> f64 {
    const LABEL_PX: f64 = 80.0;
    const STEPS: [f64; 19] = [0.001, 0.002, 0.005, 0.01, 0.02, 0.05, 0.1, 0.2, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0];
    let least = span / width.max(1.0) as f64 * LABEL_PX;
    STEPS.into_iter().find(|&s| s >= least).unwrap_or(600.0)
}

/// Ticks along the bottom of the ruler, a label at each major one.
fn draw_ruler(painter: &egui::Painter, lane: Rect, v0: f64, v1: f64, x_of: &dyn Fn(f64) -> f32, label: Color32, tick: Color32) {
    let step = ruler_step(v1 - v0, lane.width());
    let minor = step / if (step / 10f64.powf(step.log10().floor())).round() == 2.0 { 4.0 } else { 5.0 };
    let font = FontId::monospace(10.0);
    for i in (v0 / minor).floor() as i64..=(v1 / minor).ceil() as i64 {
        let t = i as f64 * minor;
        if t < v0 || t > v1 {
            continue;
        }
        let x = painter.round_to_pixel_center(x_of(t));
        let major = ((t / step) - (t / step).round()).abs() < 1e-6;
        let h = if major { 7.0 } else { 3.0 };
        painter.vline(x, (lane.bottom() - h)..=lane.bottom(), Stroke::new(1.0, if major { label } else { tick }));
        if major && x + 56.0 <= lane.right() {
            painter.text(Pos2::new(x + 3.0, lane.top() + 2.0), Align2::LEFT_TOP, fmt_time(t), font.clone(), label);
        }
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

/// Dashed lines between the channel lanes, so each channel's bounds are plain.
fn draw_lane_dividers(painter: &egui::Painter, rect: Rect, channels: usize, colors: &WaveColors) {
    let lane_h = rect.height() / channels as f32;
    for ch in 1..channels {
        let y = rect.top() + lane_h * ch as f32;
        painter.add(Shape::dashed_line(&[Pos2::new(rect.left(), y), Pos2::new(rect.right(), y)], Stroke::new(1.0, colors.axis.gamma_multiply(1.6)), 6.0, 4.0));
    }
}

/// With `samples`, the columns come from them instead of the peaks' bins.
fn draw_envelope(painter: &egui::Painter, peaks: &Peaks, samples: Option<&Pcm>, rect: Rect, v0: f64, v1: f64, gain: f32, colors: &WaveColors) {
    let channels = peaks.channels.max(1) as usize;
    let lane_h = rect.height() / channels as f32;
    let cols = rect.width().max(1.0) as usize;
    for ch in 0..channels {
        let mid = rect.top() + lane_h * (ch as f32 + 0.5);
        let amp = lane_h * 0.46;
        painter.hline(rect.x_range(), mid, Stroke::new(1.0, colors.axis));
        let bins = match samples {
            Some(pcm) => sample_columns(pcm, ch, v0, v1, cols),
            None => peaks.columns(ch, v0, v1, cols),
        };
        let xs: Vec<f32> = (0..cols).map(|i| rect.left() + i as f32 + 0.5).collect();
        let top: Vec<f32> = bins.iter().map(|b| mid - (b.max * gain).clamp(-1.0, 1.0) * amp).collect();
        let bottom: Vec<f32> = bins.iter().map(|b| mid - (b.min * gain).clamp(-1.0, 1.0) * amp).collect();
        painter.add(band(&xs, &top, &bottom, colors.peak));
        let rms_top: Vec<f32> = bins.iter().map(|b| mid - (b.rms * gain).clamp(0.0, 1.0) * amp).collect();
        let rms_bottom: Vec<f32> = bins.iter().map(|b| mid + (b.rms * gain).clamp(0.0, 1.0) * amp).collect();
        painter.add(band(&xs, &rms_top, &rms_bottom, colors.rms));
        let outline: Vec<Pos2> = xs.iter().zip(&top).map(|(&x, &y)| Pos2::new(x, y)).collect();
        painter.add(Shape::line(outline, Stroke::new(1.0, colors.peak.gamma_multiply(1.3))));
        let outline: Vec<Pos2> = xs.iter().zip(&bottom).map(|(&x, &y)| Pos2::new(x, y)).collect();
        painter.add(Shape::line(outline, Stroke::new(1.0, colors.peak.gamma_multiply(1.3))));
    }
    draw_lane_dividers(painter, rect, channels, colors);
}

fn draw_samples(painter: &egui::Painter, pcm: &Pcm, rect: Rect, v0: f64, v1: f64, gain: f32, colors: &WaveColors) {
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
        let pts: Vec<Pos2> = (f0..f1).map(|f| Pos2::new(x_of(f), mid - (pcm.samples[f * channels + ch] * gain).clamp(-1.0, 1.0) * amp)).collect();
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
    draw_lane_dividers(painter, rect, channels, colors);
}

/// Draws the wave and handles its input; the response comes back so the caller can hang a
/// menu on it.
pub fn show(ui: &mut Ui, state: &mut WaveState, peaks: Option<&Peaks>, samples: Option<&Pcm>, duration: f64, playhead: Option<f64>, height: f32, colors: &WaveColors) -> (WaveEvent, egui::Response) {
    let width = ui.available_width();
    let (resp, painter) = ui.allocate_painter(Vec2::new(width, height), Sense::click_and_drag());
    let full = resp.rect;
    let lane = Rect::from_min_max(full.left_top(), Pos2::new(full.right(), full.top() + LANE_H));
    let rect = Rect::from_min_max(Pos2::new(full.left(), lane.bottom()), full.right_bottom());
    let visuals = ui.visuals().clone();
    painter.rect_filled(full, 4.0, visuals.extreme_bg_color);
    painter.rect_filled(lane, egui::CornerRadius { nw: 4, ne: 4, sw: 0, se: 0 }, visuals.faint_bg_color);

    let Some(peaks) = peaks else {
        let idle = visuals.weak_text_color();
        for s in handle(lane, rect, rect.left(), true, idle).into_iter().chain(handle(lane, rect, rect.right(), false, idle)) {
            painter.add(s);
        }
        painter.text(rect.center(), Align2::CENTER_CENTER, "no file", FontId::proportional(14.0), visuals.weak_text_color());
        return (WaveEvent::None, resp);
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
        Some(pcm) if frames_per_px * (pcm.sample_rate as f64 / peaks.sample_rate as f64) <= SAMPLE_VIEW_FRAMES_PER_PX => draw_samples(&painter, pcm, rect, v0, v1, state.gain, colors),
        _ => draw_envelope(&painter, peaks, samples.filter(|_| frames_per_px < peaks.bin_frames as f64), rect, v0, v1, state.gain, colors),
    }

    draw_ruler(&painter, lane, v0, v1, &x_of, visuals.weak_text_color(), colors.axis);

    // Selection, fades, handles. The selection's length reads on the ruler, between its
    // edges, when it fits there clear of the fade handles.
    let hovered_hit = resp.hover_pos().and_then(|p| hit(state, lane, p, duration, &x_of));
    let active = state.drag.or(hovered_hit);
    if let Some((a, b)) = state.selection {
        let xa = x_of(a).clamp(rect.left(), rect.right());
        let xb = x_of(b).clamp(rect.left(), rect.right());
        let sel = Rect::from_min_max(Pos2::new(xa, lane.top()), Pos2::new(xb, rect.bottom()));
        painter.rect_filled(sel, 0.0, colors.selection.gamma_multiply(0.18));
        let edge = |which: Drag| if active == Some(which) { Stroke::new(2.5, Color32::WHITE) } else { Stroke::new(1.0, colors.selection) };
        painter.vline(xa, full.y_range(), edge(Drag::Start));
        painter.vline(xb, full.y_range(), edge(Drag::End));
        let length = painter.layout_no_wrap(fmt_time(b - a), FontId::monospace(11.0), colors.fade);
        if length.size().x + 2.0 * HANDLE_W + 12.0 <= xb - xa {
            let at = Pos2::new((xa + xb) * 0.5 - length.size().x * 0.5, lane.center().y - length.size().y * 0.5);
            painter.rect_filled(Rect::from_min_size(at, length.size()).expand2(Vec2::new(4.0, 1.0)), 2.0, visuals.faint_bg_color);
            painter.galley(at, length, colors.fade);
        }
    }
    if let Some((span, since)) = state.flash {
        let t = since.elapsed().as_secs_f32() / FLASH_SECS;
        if t >= 1.0 {
            state.flash = None;
        } else {
            let xa = x_of(span.0).clamp(rect.left(), rect.right());
            let xb = x_of(span.1).clamp(rect.left(), rect.right());
            painter.rect_filled(Rect::from_min_max(Pos2::new(xa, rect.top()), Pos2::new(xb, rect.bottom())), 0.0, colors.flash.gamma_multiply(0.45 * flash_left(t)));
            ui.ctx().request_repaint();
        }
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
    let fill = |hot: bool| if hot { Color32::WHITE } else { colors.fade };
    for s in handle(lane, rect, x_of(a + state.fade_in), true, fill(active == Some(Drag::FadeIn))) {
        painter.add(s);
    }
    for s in handle(lane, rect, x_of(b - state.fade_out), false, fill(active == Some(Drag::FadeOut))) {
        painter.add(s);
    }

    // Playhead.
    if let Some(t) = playhead {
        if t >= v0 && t <= v1 {
            painter.vline(x_of(t), rect.y_range(), Stroke::new(1.5, colors.playhead));
        }
    }

    // Cursor. Ctrl over the wave means a drag would carry the selection out, as in a file
    // manager.
    let ctrl = ui.input(|i| i.modifiers.ctrl);
    match active {
        _ if ctrl && resp.hovered() => ui.ctx().set_cursor_icon(CursorIcon::Grab),
        Some(Drag::Start) | Some(Drag::End) | Some(Drag::FadeIn) | Some(Drag::FadeOut) => ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal),
        Some(Drag::Move { .. }) => ui.ctx().set_cursor_icon(CursorIcon::Grabbing),
        _ if resp.hovered() && lane.contains(resp.hover_pos().unwrap_or(Pos2::ZERO)) => {}
        _ if resp.hovered() => ui.ctx().set_cursor_icon(CursorIcon::Text),
        _ => {}
    }

    // Left-button interaction.
    let mut event = WaveEvent::None;
    if resp.drag_started_by(PointerButton::Primary) && ctrl {
        state.drag = None;
        return (WaveEvent::DragOut, resp);
    }
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
                Drag::FadeIn => {
                    state.fade_in = (t - state.bounds(duration).0).max(0.0);
                    state.last_fade = Some(Fade::In);
                }
                Drag::FadeOut => {
                    state.fade_out = (state.bounds(duration).1 - t).max(0.0);
                    state.last_fade = Some(Fade::Out);
                }
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
    (event, resp)
}

pub struct GhostColors {
    pub card: Color32,
    pub peak: Color32,
    pub rms: Color32,
}

/// The drag picture: the span's envelope, all channels folded into one, on a rounded
/// translucent card of 200 by 56 logical pixels, drawn at `scale` buffer pixels per logical.
pub fn ghost(peaks: &Peaks, span: (f64, f64), gain: f32, colors: &GhostColors, scale: i32) -> crate::dnd::Image {
    let (w, h) = (200 * scale as usize, 56 * scale as usize);
    let radius = 6.0 * scale as f32;
    let mut rgba = vec![0u8; w * h * 4];
    let mut put = |x: usize, y: usize, c: Color32| {
        let i = (y * w + x) * 4;
        rgba[i..i + 4].copy_from_slice(&[c.r(), c.g(), c.b(), c.a()]);
    };
    let card = Color32::from_rgba_unmultiplied(colors.card.r(), colors.card.g(), colors.card.b(), 200);
    for y in 0..h {
        for x in 0..w {
            let (cx, cy) = ((x as f32 + 0.5 - radius).min(0.0).max(x as f32 + 0.5 - (w as f32 - radius)), (y as f32 + 0.5 - radius).min(0.0).max(y as f32 + 0.5 - (h as f32 - radius)));
            if cx * cx + cy * cy <= radius * radius {
                put(x, y, card);
            }
        }
    }
    let inset = 4 * scale as usize;
    let cols = w - 2 * inset;
    let mut merged = vec![hark_core::peaks::Bin { min: 0.0, max: 0.0, rms: 0.0 }; cols];
    for ch in 0..peaks.channels as usize {
        for (m, b) in merged.iter_mut().zip(peaks.columns(ch, span.0, span.1, cols)) {
            m.min = m.min.min(b.min);
            m.max = m.max.max(b.max);
            m.rms = m.rms.max(b.rms);
        }
    }
    let mid = h as f32 * 0.5;
    let half = mid - inset as f32;
    let peak = Color32::from_rgba_unmultiplied(colors.peak.r(), colors.peak.g(), colors.peak.b(), 230);
    let rms = Color32::from_rgba_unmultiplied(colors.rms.r(), colors.rms.g(), colors.rms.b(), 240);
    for (k, b) in merged.iter().enumerate() {
        let x = inset + k;
        let y_of = |v: f32| (mid - (v * gain).clamp(-1.0, 1.0) * half).round() as usize;
        let (top, bottom) = (y_of(b.max).min(y_of(b.min)), y_of(b.min).max(y_of(b.max)));
        for y in top..=bottom.min(h - 1) {
            put(x, y, peak);
        }
        let (rt, rb) = (y_of(b.rms).min(y_of(-b.rms)), y_of(-b.rms).max(y_of(b.rms)));
        for y in rt..=rb.min(h - 1) {
            put(x, y, rms);
        }
    }
    crate::dnd::Image { width: w as u32, height: h as u32, rgba, scale }
}

pub fn fmt_time(s: f64) -> String {
    let s = s.max(0.0);
    if s >= 60.0 {
        format!("{}:{:06.3}", (s / 60.0).floor() as u64, s % 60.0)
    } else {
        format!("{s:.3}")
    }
}

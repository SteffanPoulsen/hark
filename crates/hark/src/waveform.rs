//! The bottom waveform: whole-file envelope, zoomable view, a draggable selection, fade handles
//! in their own lane above the wave so nothing else competes for the grab, and the playhead.

use egui::{Align2, Color32, CursorIcon, FontId, Pos2, Rect, Sense, Shape, Stroke, StrokeKind, Ui, Vec2};
use hark_core::pcm::FadeShape;
use hark_core::Peaks;

const LANE_H: f32 = 20.0;
const HANDLE_W: f32 = 14.0;
const HANDLE_GRAB_PX: f32 = 16.0;
const EDGE_GRAB_PX: f32 = 8.0;

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
}

pub enum WaveEvent {
    None,
    Seek(f64),
}

impl WaveState {
    pub fn reset(&mut self, duration: f64) {
        self.view = (0.0, duration.max(1e-3));
        self.selection = None;
        self.fade_in = 0.0;
        self.fade_out = 0.0;
        self.drag = None;
    }

    pub fn clamp_fades(&mut self) {
        if let Some((a, b)) = self.selection {
            let len = (b - a).max(0.0);
            self.fade_in = self.fade_in.clamp(0.0, len);
            self.fade_out = self.fade_out.clamp(0.0, len - self.fade_in);
        }
    }

    pub fn zoom_to_selection(&mut self) {
        if let Some((a, b)) = self.selection {
            if b > a {
                let pad = (b - a) * 0.05;
                self.view = (a - pad, b + pad);
            }
        }
    }
}

/// What the pointer would grab at `p`, before any drag starts.
fn hit(state: &WaveState, lane: Rect, p: Pos2, x_of: &dyn Fn(f64) -> f32) -> Option<Drag> {
    let (a, b) = state.selection?;
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
    if (p.x - x_of(a)).abs() <= EDGE_GRAB_PX {
        Some(Drag::Start)
    } else if (p.x - x_of(b)).abs() <= EDGE_GRAB_PX {
        Some(Drag::End)
    } else {
        None
    }
}

pub fn show(ui: &mut Ui, state: &mut WaveState, peaks: Option<&Peaks>, duration: f64, playhead: Option<f64>, height: f32) -> WaveEvent {
    let width = ui.available_width();
    let (resp, painter) = ui.allocate_painter(Vec2::new(width, height), Sense::click_and_drag());
    let full = resp.rect;
    let lane = Rect::from_min_max(full.left_top(), Pos2::new(full.right(), full.top() + LANE_H));
    let rect = Rect::from_min_max(Pos2::new(full.left(), lane.bottom()), full.right_bottom());
    let visuals = ui.visuals();
    painter.rect_filled(full, 3.0, visuals.extreme_bg_color);
    painter.rect_filled(lane, 0.0, visuals.faint_bg_color);

    let Some(peaks) = peaks else {
        painter.text(rect.center(), Align2::CENTER_CENTER, "no file", FontId::proportional(14.0), visuals.weak_text_color());
        return WaveEvent::None;
    };
    let duration = duration.max(1e-6);
    if state.view.1 <= state.view.0 {
        state.view = (0.0, duration);
    }

    // Zoom and pan with the wheel while hovering, against the view as it was.
    if resp.hovered() {
        let (v0, v1) = state.view;
        let span = v1 - v0;
        let (zoom, scroll) = ui.input(|i| (i.zoom_delta(), i.smooth_scroll_delta));
        if zoom != 1.0 {
            if let Some(p) = resp.hover_pos() {
                let anchor = v0 + ((p.x - rect.left()) / rect.width()) as f64 * span;
                let new_span = (span / zoom as f64).clamp(0.002, duration);
                let frac = (anchor - v0) / span;
                let a = (anchor - frac * new_span).clamp(0.0, duration - new_span);
                state.view = (a, a + new_span);
            }
        } else if scroll.x != 0.0 || scroll.y != 0.0 {
            let delta = if scroll.x != 0.0 { scroll.x } else { scroll.y };
            let dt = -(delta / rect.width()) as f64 * span;
            let a = (v0 + dt).clamp(0.0, duration - span);
            state.view = (a, a + span);
        }
    }
    let (v0, v1) = state.view;
    let span = v1 - v0;
    let x_of = |t: f64| rect.left() + ((t - v0) / span) as f32 * rect.width();
    let t_of = |x: f32| v0 + ((x - rect.left()) / rect.width()) as f64 * span;

    // Envelope.
    let channels = peaks.channels.max(1) as usize;
    let lane_h = rect.height() / channels as f32;
    let cols = rect.width().max(1.0) as usize;
    let wave_color = Color32::from_rgb(96, 190, 214);
    let dim = Color32::from_rgb(60, 110, 125);
    for ch in 0..channels {
        let mid = rect.top() + lane_h * (ch as f32 + 0.5);
        let amp = lane_h * 0.47;
        painter.hline(rect.x_range(), mid, Stroke::new(1.0, dim.gamma_multiply(0.5)));
        for (i, (lo, hi)) in peaks.columns(ch, v0, v1, cols).into_iter().enumerate() {
            let x = rect.left() + i as f32 + 0.5;
            let y0 = mid - hi.clamp(-1.0, 1.0) * amp;
            let y1 = mid - lo.clamp(-1.0, 1.0) * amp;
            painter.line_segment([Pos2::new(x, y0), Pos2::new(x, y1.max(y0 + 1.0))], Stroke::new(1.0, wave_color));
        }
    }

    // Selection, fades, handles.
    let hovered_hit = resp.hover_pos().and_then(|p| hit(state, lane, p, &x_of));
    let active = state.drag.or(hovered_hit);
    if let Some((a, b)) = state.selection {
        let sel_color = Color32::from_rgb(255, 200, 90);
        let xa = x_of(a).clamp(rect.left(), rect.right());
        let xb = x_of(b).clamp(rect.left(), rect.right());
        let sel = Rect::from_min_max(Pos2::new(xa, rect.top()), Pos2::new(xb, rect.bottom()));
        painter.rect_filled(sel, 0.0, Color32::from_rgba_unmultiplied(255, 200, 90, 36));
        let edge = |which: Drag| if active == Some(which) { Stroke::new(2.5, Color32::WHITE) } else { Stroke::new(1.0, sel_color) };
        painter.vline(xa, rect.y_range(), edge(Drag::Start));
        painter.vline(xb, rect.y_range(), edge(Drag::End));

        let fade_color = Color32::from_rgb(255, 230, 140);
        let curve = |from: f64, to: f64, rising: bool| {
            let n = 24;
            let pts: Vec<Pos2> = (0..=n)
                .map(|i| {
                    let t = i as f32 / n as f32;
                    let g = state.fade_shape.gain(if rising { t } else { 1.0 - t });
                    Pos2::new(x_of(from + (to - from) * t as f64), rect.bottom() - g * rect.height())
                })
                .collect();
            Shape::line(pts, Stroke::new(1.5, fade_color))
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
            let fill = if hot { Color32::WHITE } else { fade_color };
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
        painter.text(lane.left_center() + Vec2::new(6.0, 0.0), Align2::LEFT_CENTER, "fades", FontId::proportional(10.0), visuals.weak_text_color());
    }

    // Playhead.
    if let Some(t) = playhead {
        if t >= v0 && t <= v1 {
            painter.vline(x_of(t), rect.y_range(), Stroke::new(1.5, Color32::from_rgb(255, 80, 80)));
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
            Color32::from_rgb(255, 220, 140),
        );
    }
    painter.rect_stroke(full, 3.0, Stroke::new(1.0, visuals.widgets.noninteractive.bg_stroke.color), StrokeKind::Inside);

    // Cursor.
    match active {
        Some(Drag::Start) | Some(Drag::End) | Some(Drag::FadeIn) | Some(Drag::FadeOut) => ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal),
        Some(Drag::Move { .. }) => ui.ctx().set_cursor_icon(CursorIcon::Grabbing),
        _ if resp.hovered() && lane.contains(resp.hover_pos().unwrap_or(Pos2::ZERO)) => {}
        _ if resp.hovered() => ui.ctx().set_cursor_icon(CursorIcon::Text),
        _ => {}
    }

    // Interaction.
    let mut event = WaveEvent::None;
    if resp.drag_started() {
        if let Some(p) = resp.interact_pointer_pos() {
            let t = t_of(p.x).clamp(0.0, duration);
            state.drag = match hit(state, lane, p, &x_of) {
                Some(d) => Some(d),
                None if lane.contains(p) => None,
                None => match state.selection {
                    Some((a, b)) if t > a && t < b && ui.input(|i| i.modifiers.shift) => Some(Drag::Move { grab: t - a }),
                    _ => Some(Drag::New { anchor: t }),
                },
            };
        }
    }
    if resp.dragged() {
        if let (Some(drag), Some(p)) = (state.drag, resp.interact_pointer_pos()) {
            let t = t_of(p.x).clamp(0.0, duration);
            match drag {
                Drag::New { anchor } => {
                    let (a, b) = if t < anchor { (t, anchor) } else { (anchor, t) };
                    state.selection = Some((a, b));
                }
                Drag::Start => {
                    if let Some((_, b)) = state.selection {
                        state.selection = Some((t.min(b), b));
                    }
                }
                Drag::End => {
                    if let Some((a, _)) = state.selection {
                        state.selection = Some((a, t.max(a)));
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
                    if let Some((a, _)) = state.selection {
                        state.fade_in = (t - a).max(0.0);
                    }
                }
                Drag::FadeOut => {
                    if let Some((_, b)) = state.selection {
                        state.fade_out = (b - t).max(0.0);
                    }
                }
            }
            state.clamp_fades();
        }
    }
    if resp.drag_stopped() {
        if let Some((a, b)) = state.selection {
            if b - a < 1e-4 {
                state.selection = None;
            }
        }
        state.drag = None;
    }
    if resp.clicked() {
        if let Some(p) = resp.interact_pointer_pos() {
            if !lane.contains(p) {
                event = WaveEvent::Seek(t_of(p.x).clamp(0.0, duration));
            }
        }
    }
    if resp.double_clicked() {
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

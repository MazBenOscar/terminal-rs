//! Spring animations (SwiftUI-like), integrated per frame and keyed by egui `Id`, plus
//! timed transitions and easing curves. Everything snaps when Reduce Motion is on.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use egui::{Context, Id};

static REDUCED: AtomicBool = AtomicBool::new(false);
static KEYS: AtomicU64 = AtomicU64::new(1);

/// A key no other [`progress`] transition has used, so replacing an object (a pane
/// reloaded under the same id) never mistakes its transition for a finished one.
pub fn fresh_key() -> u64 {
    KEYS.fetch_add(1, Ordering::Relaxed)
}

/// Whether the system asks for reduced motion.
pub fn reduced() -> bool {
    REDUCED.load(Ordering::Relaxed)
}

pub fn set_reduced(on: bool) {
    REDUCED.store(on, Ordering::Relaxed);
}

pub fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3)
}

/// Ease out with a slight overshoot past 1 before settling.
pub fn ease_out_back(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    let (c1, c3) = (1.70158, 2.70158);
    1.0 + c3 * (t - 1.0).powi(3) + c1 * (t - 1.0).powi(2)
}

/// `t` rescaled so `a..b` maps to 0..1 (clamped), for staggering parts of one transition.
pub fn span(t: f32, a: f32, b: f32) -> f32 {
    ((t - a) / (b - a)).clamp(0.0, 1.0)
}

/// Linear progress (0..1) of a `secs`-long transition that restarts whenever `key`
/// changes. The first call for an `id` starts it too. Requests repaints until done.
pub fn progress(ctx: &Context, id: Id, key: u64, secs: f32) -> f32 {
    if reduced() {
        return 1.0;
    }
    let now = ctx.input(|i| i.time);
    let (k, start) = ctx.data_mut(|d| *d.get_temp_mut_or_insert_with(id, || (key, now)));
    let start = if k == key {
        start
    } else {
        ctx.data_mut(|d| d.insert_temp(id, (key, now)));
        now
    };
    since(ctx, now - start, secs)
}

/// Entrance progress (0..1) of something drawn while a condition holds (a popover, an
/// overlay): it restarts whenever `id` was not shown on the previous frame.
pub fn appear(ctx: &Context, id: Id, secs: f32) -> f32 {
    let frame = ctx.cumulative_frame_nr();
    let now = ctx.input(|i| i.time);
    let start = match ctx.data(|d| d.get_temp::<(u64, f64)>(id)) {
        Some((last, start)) if last + 1 >= frame => start,
        _ => now,
    };
    ctx.data_mut(|d| d.insert_temp(id, (frame, start)));
    since(ctx, now - start, secs)
}

/// Progress of a transition that began `elapsed` seconds ago.
pub fn since(ctx: &Context, elapsed: f64, secs: f32) -> f32 {
    if reduced() {
        return 1.0;
    }
    let t = (elapsed / secs.max(1e-3) as f64).clamp(0.0, 1.0) as f32;
    if t < 1.0 {
        ctx.request_repaint();
    }
    t
}

/// A damped spring moving `pos` toward a target.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spring {
    pub pos: f32,
    pub vel: f32,
}

/// Response and damping of the default UI spring (slightly under-damped, like
/// SwiftUI's `.spring(response: 0.35, dampingFraction: 0.82)`).
pub const RESPONSE: f32 = 0.35;
pub const DAMPING: f32 = 0.82;

impl Spring {
    pub fn at(pos: f32) -> Self {
        Self { pos, vel: 0.0 }
    }

    /// Advance by `dt` seconds toward `target`. `response` is the undamped period in
    /// seconds and `damping` the damping fraction (1 = critically damped).
    pub fn step(&mut self, target: f32, dt: f32, response: f32, damping: f32) {
        let omega = std::f32::consts::TAU / response.max(1e-3);
        let k = omega * omega;
        let c = 2.0 * damping * omega;
        // Sub-step for stability with long frames.
        let n = (dt / (1.0 / 240.0)).ceil().clamp(1.0, 32.0) as usize;
        let h = dt / n as f32;
        for _ in 0..n {
            let acc = -k * (self.pos - target) - c * self.vel;
            self.vel += acc * h;
            self.pos += self.vel * h;
        }
    }

    pub fn settled(&self, target: f32, scale: f32) -> bool {
        let eps = 1e-3 * scale.abs().max(1.0);
        (self.pos - target).abs() < eps && self.vel.abs() < eps * 10.0
    }
}

/// Animate a value toward `target` with the default spring; returns the current value.
/// The first call snaps to the target. Requests repaints until the spring settles.
pub fn spring(ctx: &Context, id: Id, target: f32) -> f32 {
    spring_with(ctx, id, target, RESPONSE, DAMPING)
}

pub fn spring_with(ctx: &Context, id: Id, target: f32, response: f32, damping: f32) -> f32 {
    let dt = ctx.input(|i| i.stable_dt).clamp(0.0, 0.1);
    let mut s = ctx.data_mut(|d| *d.get_temp_mut_or_insert_with(id, || Spring::at(target)));
    if reduced() || s.settled(target, target) {
        s = Spring::at(target);
    } else {
        s.step(target, dt, response, damping);
        if s.settled(target, target) {
            s = Spring::at(target);
        } else {
            ctx.request_repaint();
        }
    }
    ctx.data_mut(|d| d.insert_temp(id, s));
    s.pos
}

/// Spring between 0 and 1 for a boolean (hover/focus fades, panel open/closed).
pub fn spring_bool(ctx: &Context, id: Id, on: bool) -> f32 {
    spring_with(ctx, id, if on { 1.0 } else { 0.0 }, 0.25, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converges_with_small_overshoot() {
        let mut s = Spring::at(0.0);
        let mut max = 0.0f32;
        for _ in 0..240 {
            s.step(100.0, 1.0 / 120.0, RESPONSE, DAMPING);
            max = max.max(s.pos);
        }
        assert!((s.pos - 100.0).abs() < 0.5, "{}", s.pos);
        assert!(max > 100.0 && max < 106.0, "overshoot {max}");
        let mut c = Spring::at(0.0);
        let mut cmax = 0.0f32;
        for _ in 0..240 {
            c.step(1.0, 1.0 / 60.0, 0.25, 1.0);
            cmax = cmax.max(c.pos);
        }
        assert!(cmax <= 1.0 + 1e-3, "critically damped must not overshoot: {cmax}");
    }

    #[test]
    fn long_frames_stay_stable() {
        let mut s = Spring::at(0.0);
        s.step(1.0, 0.1, RESPONSE, DAMPING);
        assert!(s.pos.is_finite() && s.pos.abs() < 2.0);
    }

    #[test]
    fn easings_hit_their_ends() {
        for f in [ease_out, ease_out_back] {
            assert!(f(0.0).abs() < 1e-6);
            assert!((f(1.0) - 1.0).abs() < 1e-6);
        }
        let peak = (0..=100).map(|i| ease_out_back(i as f32 / 100.0)).fold(0.0, f32::max);
        assert!(peak > 1.05 && peak < 1.15, "overshoot {peak}");
        assert!(ease_out(0.5) > 0.5);
        assert_eq!(span(0.5, 0.25, 0.75), 0.5);
        assert_eq!(span(0.1, 0.25, 0.75), 0.0);
    }

    #[test]
    fn progress_restarts_when_the_key_changes() {
        let ctx = Context::default();
        let id = Id::new("p");
        let frame = |key: u64, t: f64| {
            let input = egui::RawInput { time: Some(t), ..Default::default() };
            let mut out = 0.0;
            ctx.run_ui(input, |ui| out = progress(ui.ctx(), id, key, 0.5)).textures_delta.clear();
            out
        };
        assert_eq!(frame(1, 10.0), 0.0);
        assert!((frame(1, 10.25) - 0.5).abs() < 1e-4);
        assert_eq!(frame(1, 11.0), 1.0);
        assert_eq!(frame(2, 12.0), 0.0);
        assert!((frame(2, 12.1) - 0.2).abs() < 1e-4);
    }

    #[test]
    fn appear_restarts_after_a_hidden_frame() {
        let ctx = Context::default();
        let id = Id::new("a");
        let frame = |t: f64, show: bool| {
            let input = egui::RawInput { time: Some(t), ..Default::default() };
            let mut out = None;
            ctx.run_ui(input, |ui| {
                if show {
                    out = Some(appear(ui.ctx(), id, 0.2));
                }
            })
            .textures_delta
            .clear();
            out
        };
        assert_eq!(frame(1.0, true), Some(0.0));
        assert!((frame(1.1, true).unwrap() - 0.5).abs() < 1e-4);
        assert_eq!(frame(1.3, true), Some(1.0));
        frame(1.4, false);
        assert_eq!(frame(1.5, true), Some(0.0), "reopened");
    }

    #[test]
    fn context_helper_snaps_then_animates() {
        let ctx = Context::default();
        let id = Id::new("x");
        assert_eq!(spring(&ctx, id, 5.0), 5.0);
        ctx.run_ui(Default::default(), |_| {}).textures_delta.clear();
        let v = spring(&ctx, id, 10.0);
        assert!((5.0..=10.5).contains(&v));
    }
}

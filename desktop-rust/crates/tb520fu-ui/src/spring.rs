// SPDX-License-Identifier: MIT
/// Critically-damped / slightly-overdamped 1D spring for 144Hz interruptible UI motion.
#[derive(Debug, Clone, Copy)]
pub struct Spring1D {
    pub pos: f32,
    pub vel: f32,
    pub target: f32,
    /// Angular frequency omega_0 (rad/s). ~28.0 gives snappy ~200ms OriginOS feel.
    pub omega: f32,
    /// Damping ratio zeta. 1.0 = critically damped (zero overshoot), 0.88 = slight organic bounce.
    pub zeta: f32,
    pub epsilon: f32,
}

impl Spring1D {
    pub const fn new(initial: f32) -> Self {
        Self {
            pos: initial,
            vel: 0.0,
            target: initial,
            omega: 30.0,
            zeta: 1.0,
            epsilon: 0.35,
        }
    }

    pub const fn with_params(initial: f32, omega: f32, zeta: f32) -> Self {
        Self {
            pos: initial,
            vel: 0.0,
            target: initial,
            omega,
            zeta,
            epsilon: 0.35,
        }
    }

    /// Critically-damped / organic spring for `0.0..=1.0` normalized progress values
    /// (`epsilon = 0.0008`, i.e. `< 1 px` on a `1150 px` screen so the final 35% deceleration tail
    /// never teleports or stalls).
    pub const fn normalized(initial: f32, omega: f32, zeta: f32) -> Self {
        Self {
            pos: initial,
            vel: 0.0,
            target: initial,
            omega,
            zeta,
            epsilon: 0.0008,
        }
    }

    /// Snap immediately without animating (e.g., while tracking a live finger drag).
    pub fn snap(&mut self, value: f32) {
        self.pos = value;
        self.target = value;
        self.vel = 0.0;
    }

    /// Set a new target while preserving existing velocity (smooth interruption).
    pub fn set_target(&mut self, target: f32) {
        self.target = target;
    }

    /// Inject release velocity (px/s) and target on gesture lift.
    pub fn fling(&mut self, target: f32, velocity: f32) {
        self.target = target;
        self.vel = velocity;
    }

    pub fn is_animating(&self) -> bool {
        (self.pos - self.target).abs() > self.epsilon || self.vel.abs() > self.epsilon * 4.0
    }

    /// Advance by `dt` seconds using semi-implicit Euler sub-stepping for unconditional stability.
    /// Returns `true` whenever `pos` moved (including the final settle frame onto `target`), and
    /// `false` once completely at rest on `target`.
    pub fn step(&mut self, dt: f32) -> bool {
        if !self.is_animating() {
            let changed = (self.pos - self.target).abs() > 1e-6 || self.vel.abs() > 1e-6;
            self.pos = self.target;
            self.vel = 0.0;
            return changed;
        }
        let dt = dt.clamp(0.0005, 0.05);
        // Sub-step at <= 4ms so high omega never explodes on a dropped frame
        let steps = ((dt / 0.004).ceil() as usize).max(1);
        let h = dt / (steps as f32);
        let k = self.omega * self.omega;
        let c = 2.0 * self.zeta * self.omega;
        for _ in 0..steps {
            let disp = self.pos - self.target;
            let acc = -k * disp - c * self.vel;
            self.vel += acc * h;
            self.pos += self.vel * h;
        }
        if !self.is_animating() {
            self.pos = self.target;
            self.vel = 0.0;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spring_converges_cleanly() {
        let mut s = Spring1D::new(0.0);
        s.set_target(400.0);
        let mut ticks = 0;
        while s.step(1.0 / 144.0) {
            ticks += 1;
            assert!(ticks < 200, "spring took too long to settle");
            assert!(s.pos <= 401.0, "critically damped spring must not overshoot");
        }
        assert_eq!(s.pos, 400.0);
    }

    #[test]
    fn normalized_spring_emits_final_target_frame_and_never_stalls_at_90_percent() {
        // 1. Full 0 -> 1 transition preserves smooth deceleration tail above 0.90 (no 0.65 -> 1.0 jump)
        let mut s = Spring1D::normalized(0.0, 19.0, 1.0);
        s.set_target(1.0);
        let mut saw_above_90 = false;
        let mut last_drawn_pos = 0.0;
        let mut ticks = 0;
        while s.step(1.0 / 120.0) {
            ticks += 1;
            if s.pos > 0.90 && s.pos < 0.999 {
                saw_above_90 = true;
            }
            last_drawn_pos = s.pos;
            assert!(ticks < 240);
        }
        assert!(saw_above_90, "normalized spring must smoothly traverse 90%..99.9% tail");
        assert_eq!(last_drawn_pos, 1.0, "final settle frame must return true with exact target 1.0");
        assert!(!s.step(1.0 / 120.0), "subsequent idle ticks must return false");

        // 2. Lifting finger at 90% (`snap(0.90) -> set_target(1.0)`) must animate all the way to 1.0
        s.snap(0.90);
        s.set_target(1.0);
        assert!(s.is_animating(), "90% -> 100% must be recognized as animating");
        let mut steps_90 = 0;
        let mut final_pos = s.pos;
        while s.step(1.0 / 120.0) {
            steps_90 += 1;
            final_pos = s.pos;
            assert!(steps_90 < 180);
        }
        assert!(steps_90 >= 4, "90% -> 100% should glide smoothly over multiple frames");
        assert_eq!(final_pos, 1.0);
    }
}

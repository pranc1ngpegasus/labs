//! Comfort noise for echo-only periods.

/// Tracks near-end noise floor and synthesizes low-level white noise.
#[derive(Debug, Clone)]
pub(super) struct ComfortNoise {
    enabled: bool,
    noise_floor: f32,
    /// Simple LCG state for reproducible white noise (no OS entropy needed).
    rng: u32,
}

impl ComfortNoise {
    pub(super) const fn new(enabled: bool) -> Self {
        Self {
            enabled,
            noise_floor: 1e-4,
            rng: 0xC0_FF_EE_u32,
        }
    }

    pub(super) const fn noise_floor(&self) -> f32 {
        self.noise_floor
    }

    pub(super) const fn reset(&mut self) {
        self.noise_floor = 1e-4;
        self.rng = 0xC0_FF_EE_u32;
    }

    /// Exponentially smooth the floor while the near-end is quiet.
    pub(super) fn observe_near(
        &mut self,
        near: f32,
    ) {
        let level = near.abs();
        // Only learn from quiet frames so speech does not inflate the floor.
        if level < self.noise_floor * 4.0 || level < 1e-3 {
            self.noise_floor = 0.005f32.mul_add(level, 0.995 * self.noise_floor);
        }
    }

    /// Mix comfort noise into an echo-cancelled sample when appropriate.
    pub(super) fn maybe_mix(
        &mut self,
        residual: f32,
        echo_only: bool,
    ) -> f32 {
        if !(self.enabled && echo_only) {
            return residual;
        }
        self.noise_floor.mul_add(self.white(), residual)
    }

    fn white(&mut self) -> f32 {
        // Numerical Recipes LCG → uniform in (-1, 1).
        // Use the top 24 bits so the value fits the `f32` mantissa exactly.
        self.rng = self.rng.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let bits = self.rng >> 8;
        #[allow(
            clippy::cast_precision_loss,
            reason = "24-bit integer fits f32 mantissa"
        )]
        let unit = bits as f32 / 16_777_216.0_f32; // 2^24
        unit.mul_add(2.0, -1.0)
    }
}

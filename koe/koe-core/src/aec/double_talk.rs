//! Geigel double-talk detector.

/// Default hangover in samples after a Geigel trigger (~10 ms at 48 kHz).
const DEFAULT_HANGOVER: usize = 480;

/// Geigel algorithm: near-end is “talking” when its amplitude exceeds a
/// threshold times the peak far-end amplitude in the filter window.
///
/// A short hangover keeps adaptation frozen across zero-crossings of speech
/// so sample-by-sample Geigel does not leak updates between peaks.
#[derive(Debug, Clone)]
pub(super) struct GeigelDetector {
    /// Linear amplitude ratio derived from the configured dB threshold.
    threshold: f32,
    hangover_samples: usize,
    hangover_remaining: usize,
}

impl GeigelDetector {
    pub(super) fn from_db(threshold_db: f32) -> Self {
        Self {
            threshold: 10.0_f32.powf(threshold_db / 20.0),
            hangover_samples: DEFAULT_HANGOVER,
            hangover_remaining: 0,
        }
    }

    pub(super) const fn reset(&mut self) {
        self.hangover_remaining = 0;
    }

    /// Returns `true` when adaptation should freeze.
    #[inline]
    pub(super) fn is_double_talk(
        &mut self,
        near: f32,
        max_abs_far: f32,
    ) -> bool {
        if near.abs() > self.threshold * max_abs_far {
            self.hangover_remaining = self.hangover_samples;
        }
        if self.hangover_remaining > 0 {
            self.hangover_remaining -= 1;
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn triggers_above_threshold_and_holds_hangover() {
        let mut d = GeigelDetector::from_db(6.0); // ≈ 2.0×
        assert!(!d.is_double_talk(0.5, 1.0)); // 0.5 < 2.0
        assert!(d.is_double_talk(2.1, 1.0)); // 2.1 > 2.0
        // Hangover keeps freeze even when mic drops.
        assert!(d.is_double_talk(0.0, 1.0));
    }

    #[test]
    fn reset_clears_hangover() {
        let mut d = GeigelDetector::from_db(6.0);
        assert!(d.is_double_talk(3.0, 1.0));
        d.reset();
        assert!(!d.is_double_talk(0.5, 1.0));
    }
}

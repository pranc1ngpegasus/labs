---
title: 21 — Echo Cancellation (AEC)
status: draft
depends: [15-pipeline-core]
spec_refs: [05-echo-cancellation]
---

# 21 — Acoustic Echo Cancellation

Implement NLMS-based AEC with double-talk detection and comfort noise.

## Location

`koe-core/src/aec/`

```
koe-core/src/aec/
  mod.rs         — AcousticEchoCanceller struct + AecConfig
  nlms.rs        — NLMS filter implementation
  double_talk.rs — Geigel double-talk detector
  comfort.rs     — Comfort noise generator
```

## AecConfig

```rust
pub struct AecConfig {
    pub filter_length: usize,     // 4096 taps (~85 ms at 48 kHz)
    pub block_size: usize,        // 256 samples (~5.3 ms)
    pub step_size: f32,           // 0.01 (normalized)
    pub double_talk_threshold_db: f32, // 6.0 dB
    pub comfort_noise: bool,      // default: true
}
```

## AcousticEchoCanceller

```rust
pub struct AcousticEchoCanceller {
    filter: Vec<f32>,            // Adaptive filter taps
    ref_buffer: VecDeque<f32>,   // Recent far-end samples
    noise_floor: f32,
    config: AecConfig,
}

impl AcousticEchoCanceller {
    pub fn new(config: AecConfig) -> Self;

    /// Process one audio block.
    /// `far_end`  — system audio (reference signal)
    /// `near_end` — microphone (to be cleaned)
    /// Returns echo-cancelled near-end signal.
    pub fn process_block(&mut self, far_end: &[f32], near_end: &[f32]) -> Vec<f32>;

    pub fn reset(&mut self);
    pub fn erle(&self) -> f32;  // Echo Return Loss Enhancement, dB
}
```

## NLMS Algorithm

1. Compute estimated echo: `y[n] = sum(w[k] * x[n-k])` for k in 0..N
2. Compute error: `e[n] = d[n] - y[n]`  (near-end minus estimate)
3. If double-talk NOT detected: update weights `w[k] = w[k] + mu * e[n] * x[n-k] / (||x||² + eps)`
4. If double-talk detected: freeze adaptation, still filter
5. If comfort noise enabled and echo-only period: mix noise_floor * white_noise

## Double-Talk Detection (Geigel)

```
double_talk = (|mic[n]| > threshold * max(|ref[n]|, ..., |ref[n-N]|))
```
Threshold = 10^(6/20) ≈ 2.0 (for 6 dB threshold).

## Performance Budget

| Metric | Budget |
|--------|--------|
| CPU per block (256 samples) | < 50 µs |
| Memory | < 64 KB |
| Added latency | < 6 ms |
| ERLE target | > 20 dB |

## Verification

### Synthetic Test
1. Feed known far-end signal
2. Mix with near-end silence
3. Verify cancellation (output ~= silence)
4. Measure ERLE (must be > 20 dB)

### Double-Talk Test
1. Feed far-end + near-end simultaneously
2. Verify adaptation freezes during double-talk
3. Verify filtering still occurs with frozen coefficients

### Convergence Test
1. Start with zero filter coefficients
2. Feed constant far-end signal
3. Verify convergence in < 1 second

---
title: 42 — Pipeline Metrics
status: draft
depends: [15-pipeline-core]
spec_refs: [10-recording-pipeline]
---

# 42 — Internal Pipeline Metrics

Collect and expose internal metrics for debugging and performance monitoring.

## Location

`koe-core/src/pipeline/metrics.rs`

## Data Type

```rust
#[derive(Debug, Clone)]
pub struct PipelineMetrics {
    pub total_frames_processed: u64,
    pub dropped_frames: u64,
    pub aec_erle_db: f32,          // Echo Return Loss Enhancement
    pub encoder_bitrate_bps: f32,
    pub speech_segment_count: u64,
    pub avg_segment_latency_ms: f32, // Time from audio capture to transcript output
    pub peak_ring_buffer_usage: f32, // 0.0–1.0, fraction of capacity used
    pub consumer_lag_events: u64,
}
```

## Collection Points

1. **Ring buffer**: track `dropped_frames` from native drop counter
2. **AEC**: track ERLE from filter output
3. **Encoder**: track bytes written / duration for bitrate
4. **Speech analyzer**: track segment count and latency
5. **Consumer loop**: track lag events (broadcast channel overflow)

## Exposure

### CLI
```bash
koe record --metrics  # Prints metrics at stop
```

### GUI
- Debug menu → "Show Metrics" → overlay or print to log
- No file written unless requested

## Implementation

```rust
impl RecordingPipeline {
    pub fn metrics(&self) -> PipelineMetrics {
        PipelineMetrics {
            total_frames_processed: self.metrics.total_frames.load(Ordering::Relaxed),
            dropped_frames: self.drop_counter.load(Ordering::Relaxed),
            aec_erle_db: self.aec.as_ref().map(|a| a.erle()).unwrap_or(0.0),
            encoder_bitrate_bps: self.bytes_written as f32 * 8.0 / self.elapsed_secs(),
            speech_segment_count: self.segments.len() as u64,
            avg_segment_latency_ms: self.compute_avg_latency(),
            peak_ring_buffer_usage: self.peak_ring_usage.load(Ordering::Relaxed),
            consumer_lag_events: self.lag_events.load(Ordering::Relaxed),
        }
    }
}
```

## Verification

- Run a recording session, verify metrics are non-zero
- Artificially slow down consumer → lag events increment
- Check dropped frames when ring buffer overflows
- Verify ERLE is computed during AEC-active recordings

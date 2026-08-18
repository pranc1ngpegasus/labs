//! Pipeline runtime metrics.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Internal metrics collected during a recording session.
#[derive(Debug, Default)]
pub struct PipelineMetrics {
    total_frames_processed: AtomicU64,
    dropped_frames: AtomicU64,
    speech_segment_count: AtomicU64,
}

impl PipelineMetrics {
    /// Creates a new metrics instance with zeroed counters.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Increments the processed-frame counter by `frames`.
    pub fn record_frames(
        &self,
        frames: u64,
    ) {
        self.total_frames_processed
            .fetch_add(frames, Ordering::Relaxed);
    }

    /// Increments the dropped-frame counter by `frames`.
    pub fn record_drops(
        &self,
        frames: u64,
    ) {
        self.dropped_frames.fetch_add(frames, Ordering::Relaxed);
    }

    /// Increments the transcript segment counter.
    pub fn record_segment(&self) {
        self.speech_segment_count.fetch_add(1, Ordering::Relaxed);
    }

    /// Snapshot of current counter values.
    #[must_use]
    pub fn snapshot(&self) -> PipelineMetricsSnapshot {
        PipelineMetricsSnapshot {
            total_frames_processed: self.total_frames_processed.load(Ordering::Relaxed),
            dropped_frames: self.dropped_frames.load(Ordering::Relaxed),
            speech_segment_count: self.speech_segment_count.load(Ordering::Relaxed),
        }
    }
}

/// Point-in-time metrics values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PipelineMetricsSnapshot {
    pub total_frames_processed: u64,
    pub dropped_frames: u64,
    pub speech_segment_count: u64,
}

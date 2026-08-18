//! Consumer-side audio chunk type.

/// A block of canonical PCM audio flowing through the pipeline.
#[derive(Debug, Clone)]
pub struct AudioChunk {
    /// Interleaved stereo samples (48 kHz).
    pub samples: Vec<f32>,
    /// Monotonic timestamp in milliseconds.
    pub timestamp_ms: u64,
    /// Number of frames per channel.
    pub frame_count: usize,
}

impl AudioChunk {
    /// Creates a chunk from interleaved stereo PCM.
    #[must_use]
    #[allow(clippy::missing_const_for_fn)]
    pub fn new(
        samples: Vec<f32>,
        timestamp_ms: u64,
    ) -> Self {
        let frame_count = samples.len() / 2;
        Self {
            samples,
            timestamp_ms,
            frame_count,
        }
    }
}

//! Recording session: capture + convert + encode lifecycle and statistics.

use std::fs::File;
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use oto_capture::CaptureSession;
use oto_encode::{
    AudioEncoder, Converter, EncoderSpec, EncoderStats, Error as EncodeError, OggOpusEncoder, Tags,
    WavEncoder, convert::opus_target_channels, convert::opus_target_rate,
};
use thiserror::Error;

use crate::{CaptureError, pipeline::run_consumer};

/// Output container format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    /// Lossless WAV, preserving the captured format.
    Wav,
    /// Compressed Ogg/Opus.
    OggOpus,
}

/// Configuration for a recording session.
#[derive(Debug, Clone)]
pub struct RecordingConfig {
    /// Output file path.
    pub output: PathBuf,
    /// Output container format.
    pub format: OutputFormat,
    /// Resolved device `unique_id`, or `None` for the default input.
    pub device_id: Option<String>,
    /// Requested channel count (1 or 2); the device's actual count is used.
    pub channels: u8,
    /// Opus bitrate in bits per second (ignored for WAV).
    pub bitrate_bps: Option<u32>,
    /// Container tags (used for Ogg/Opus).
    pub tags: Tags,
}

/// Errors from a recording session.
#[derive(Debug, Error)]
pub enum RecordingError {
    /// Device enumeration or capture setup failed.
    #[error("capture error: {0}")]
    Capture(#[from] CaptureError),
    /// Encoding, converting, or finalizing failed.
    #[error("encode error: {0}")]
    Encode(#[from] EncodeError),
    /// Opening the output file failed.
    #[error("output error: {0}")]
    Output(#[from] std::io::Error),
    /// The consumer thread panicked.
    #[error("consumer thread panicked")]
    ConsumerPanicked,
}

/// An in-progress recording session.
///
/// Created via [`RecordingSession::start`], then stopped via [`Self::stop`],
/// which gracefully finalizes the output and returns statistics.
pub struct RecordingSession {
    capture: CaptureSession,
    /// Held to close the channel on stop (the consumer reads the receiver).
    sender: std::sync::mpsc::SyncSender<oto_capture::AudioFrameOwned>,
    /// Backpressure drop counter shared with the capture callback.
    dropped: Arc<AtomicUsize>,
    /// The consumer thread, yielding stats once the channel closes.
    consumer: std::thread::JoinHandle<Result<EncoderStats, RecordingError>>,
}

impl RecordingSession {
    /// Starts a recording session per `config`, opening the output file and
    /// spinning up the consumer thread.
    ///
    /// The device is opened with the requested channel count; the actual rate
    /// and channel count are read from the device after start and drive the
    /// encoder spec (design 04).
    ///
    /// # Errors
    ///
    /// Returns [`RecordingError`] when the device can't be opened/started, the
    /// output file can't be created, or the encoder can't be constructed.
    pub fn start(config: &RecordingConfig) -> Result<Self, RecordingError> {
        let file = File::create(&config.output)?;

        let (sender, receiver) = std::sync::mpsc::sync_channel::<oto_capture::AudioFrameOwned>(32);
        let dropped = Arc::new(AtomicUsize::new(0));

        let capture = CaptureSession::start(
            config.device_id.clone(),
            i32::from(config.channels),
            sender.clone(),
            Arc::clone(&dropped),
        )?;

        let actual_rate = u32::try_from(capture.sample_rate())
            .map_err(|_| EncodeError::Unsupported("sample rate exceeds u32".to_owned()))?;
        let actual_channels = u8::try_from(capture.channels())
            .map_err(|_| EncodeError::Unsupported("channel count exceeds u8".to_owned()))?;
        let (converter, encoder): (Option<Converter>, Box<dyn AudioEncoder>) = match config.format {
            OutputFormat::Wav => (
                None,
                Box::new(WavEncoder::new(
                    file,
                    EncoderSpec {
                        sample_rate: actual_rate,
                        channels: actual_channels,
                    },
                )),
            ),
            OutputFormat::OggOpus => {
                let target_rate = opus_target_rate(actual_rate);
                let target_channels = opus_target_channels(actual_channels, config.channels);
                let converter = Converter::new(actual_rate, target_rate, target_channels)
                    .map_err(EncodeError::from)?;
                let encoder = OggOpusEncoder::new(
                    file,
                    target_rate,
                    target_channels,
                    config.bitrate_bps,
                    &config.tags,
                )?;
                (Some(converter), Box::new(encoder))
            },
        };

        let consumer = std::thread::Builder::new()
            .name("oto-consumer".to_owned())
            .spawn(move || run_consumer(receiver, converter, encoder))
            .map_err(|e| EncodeError::Unsupported(format!("spawn consumer: {e}")))?;

        Ok(Self {
            capture,
            sender,
            dropped,
            consumer,
        })
    }

    /// The encoder's output spec (rate/channels) for the progress display.
    #[must_use]
    pub fn spec(&self) -> EncoderSpec {
        let rate = u32::try_from(self.capture.sample_rate()).unwrap_or(0);
        EncoderSpec {
            sample_rate: rate,
            channels: u8::try_from(self.capture.channels()).unwrap_or(0),
        }
    }

    /// Stops the capture, closes the channel, waits for the consumer to flush
    /// and finalize, and returns the recording statistics (with the actual
    /// backpressure drop count).
    ///
    /// # Errors
    ///
    /// Returns [`RecordingError`] if the consumer failed or its thread panicked.
    pub fn stop(self) -> Result<EncoderStats, RecordingError> {
        let mut capture = self.capture;
        capture.stop();
        // Dropping the capture destroys the audio session and its callback,
        // which holds the last sender clone. Only then does the channel close
        // so the consumer can drain, flush, and finalize.
        drop(capture);
        drop(self.sender);
        let mut stats = self
            .consumer
            .join()
            .map_err(|_| RecordingError::ConsumerPanicked)??;
        stats.dropped = self.dropped.load(Ordering::Relaxed) as u64;
        Ok(stats)
    }
}

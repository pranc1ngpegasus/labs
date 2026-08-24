//! Recording pipeline orchestration.

mod chunk;
mod consumer;
mod disk_check;
mod error;
mod file_writer;
mod metrics;
mod mixer;
mod monitor;
mod shutdown;

#[cfg(test)]
mod test_support;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use koe_ffi::{
    AudioCallback, AudioSourceConfig, CaptureHandle, OutputFormat, Permission, PermissionStatus,
    RecordingError, SpeechEngine, TranscriptFormat, TranscriptionCallback, TranscriptionHandle,
    TranscriptionSegment, check_permission, start_capture, start_transcription,
    validate_capture_source, validate_locale, validate_output_path,
};
use tokio::sync::{Mutex as AsyncMutex, broadcast, mpsc};
use tokio::task::JoinHandle;

use crate::codec::{AudioEncoder, OggComments, create_encoder};
use crate::transcript::{TranscriptFormatter, TranscriptMeta, create_formatter};

use chunk::AudioChunk;
use consumer::{ConsumerContext, SpeechFeeder, spawn_consumer};
pub use disk_check::available_disk_space;
use disk_check::check_disk_space;
pub use error::PipelineError;
use file_writer::FileWriter;
/// Progress payload types for [`RecordingPipeline::subscribe_progress`].
/// Segment live feed: [`RecordingPipeline::subscribe_segments`].
pub use koe_ffi::{RecordingState, RecordingStatus};
use metrics::PipelineMetrics;
pub use metrics::PipelineMetricsSnapshot;
use monitor::{AudioMonitor, start_session_monitor};
pub use shutdown::StopResult;

/// Configuration for a recording session.
///
/// Feature toggles (`enable_aec`, `comfort_noise`, `monitor`, `transcribe`) are
/// independent session flags; collapsing them into an enum would obscure that.
#[derive(Debug, Clone)]
#[allow(clippy::struct_excessive_bools)]
pub struct PipelineConfig {
    /// Audio capture source.
    pub source: AudioSourceConfig,
    /// Path for encoded audio output.
    pub output_path: PathBuf,
    /// Optional path for transcript output.
    pub transcript_output_path: Option<PathBuf>,
    /// BCP-47 locale for speech recognition (ignored when [`Self::transcribe`] is false).
    pub locale: String,
    /// Which speech engine to use (on-device / network / auto).
    ///
    /// Ignored when [`Self::transcribe`] is false.
    pub speech_engine: SpeechEngine,
    /// Encoded audio format.
    pub audio_format: OutputFormat,
    /// Transcript file format (ignored when [`Self::transcribe`] is false).
    pub transcript_format: TranscriptFormat,
    /// Enable acoustic echo cancellation (for `Both` sources).
    pub enable_aec: bool,
    /// Inject comfort noise during echo-only periods.
    pub comfort_noise: bool,
    /// Route clean audio to the default output device.
    ///
    /// When `true`, the pipeline opens a native `AudioQueue` output at start.
    /// Create failures are logged and monitoring is disabled so recording still
    /// proceeds. Write failures after start are also non-fatal.
    pub monitor: bool,
    /// Run on-device speech recognition.
    ///
    /// When `false`, ASR is skipped and [`Self::transcript_output_path`] must be
    /// `None` (validated in [`RecordingPipeline::start`]).
    pub transcribe: bool,
    /// Optional estimated recording duration for disk-space checks.
    pub estimated_duration_hours: Option<f64>,
}

/// Lifecycle state of the recording pipeline.
#[derive(Debug, Clone, Copy)]
pub enum PipelineState {
    /// Actively recording.
    Recording,
    /// Recording paused; tap remains alive.
    Paused { elapsed_before_pause: Duration },
    /// Recording has been stopped.
    Stopped,
}

/// Central orchestrator for capture, encoding, transcription, and file output.
pub struct RecordingPipeline {
    config: PipelineConfig,
    state: PipelineState,
    encoder: Arc<Mutex<Box<dyn AudioEncoder>>>,
    transcript_fmt: Arc<Mutex<Box<dyn TranscriptFormatter>>>,
    file_writer: Arc<AsyncMutex<FileWriter>>,
    capture_handles: Vec<Arc<CaptureHandle>>,
    mixer_task: Option<JoinHandle<()>>,
    transcription_handle: Option<Arc<TranscriptionHandle>>,
    consumer_task: Option<JoinHandle<Result<(), PipelineError>>>,
    shutdown: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    drop_counter: Arc<AtomicU64>,
    metrics: Arc<PipelineMetrics>,
    segments: Arc<Mutex<Vec<TranscriptionSegment>>>,
    progress_tx: broadcast::Sender<RecordingStatus>,
    segment_tx: broadcast::Sender<TranscriptionSegment>,
    /// Pause-aware origin shared with the consumer progress clock.
    started_at: Arc<Mutex<Instant>>,
    bytes_written: Arc<AtomicU64>,
    /// Live pass-through sink (`None` when monitoring is off or failed to open).
    monitor: Option<Arc<dyn AudioMonitor>>,
}

enum AudioSink {
    Mixed(broadcast::Sender<AudioChunk>),
    Side(mpsc::Sender<AudioChunk>),
}

impl AudioSink {
    fn try_push(
        &self,
        chunk: AudioChunk,
    ) -> bool {
        match self {
            Self::Mixed(tx) => tx.send(chunk).is_ok(),
            Self::Side(tx) => tx.try_send(chunk).is_ok(),
        }
    }
}

struct CaptureAudioCallback {
    sink: AudioSink,
    paused: Arc<AtomicBool>,
    drop_counter: Arc<AtomicU64>,
}

impl AudioCallback for CaptureAudioCallback {
    fn on_audio(
        &self,
        pcm: Vec<f32>,
        timestamp_ms: u64,
    ) {
        if self.paused.load(Ordering::Relaxed) {
            return;
        }
        if !self.sink.try_push(AudioChunk::new(pcm, timestamp_ms)) {
            self.drop_counter.fetch_add(1, Ordering::Relaxed);
        }
    }
}

type CaptureSetup = (Vec<Arc<CaptureHandle>>, Option<JoinHandle<()>>);

fn start_captures(
    config: &PipelineConfig,
    audio_tx: broadcast::Sender<AudioChunk>,
    paused: Arc<AtomicBool>,
    drop_counter: Arc<AtomicU64>,
    shutdown: Arc<AtomicBool>,
) -> Result<CaptureSetup, PipelineError> {
    match &config.source {
        AudioSourceConfig::Both { bundle_id } => {
            let (far_tx, far_rx) = mpsc::channel(1024);
            let (near_tx, near_rx) = mpsc::channel(1024);
            let system = start_capture(
                AudioSourceConfig::AppAudio {
                    bundle_id: bundle_id.clone(),
                },
                Box::new(CaptureAudioCallback {
                    sink: AudioSink::Side(far_tx),
                    paused: Arc::clone(&paused),
                    drop_counter: Arc::clone(&drop_counter),
                }),
            )?;
            let mic = start_capture(
                AudioSourceConfig::Microphone,
                Box::new(CaptureAudioCallback {
                    sink: AudioSink::Side(near_tx),
                    paused,
                    drop_counter,
                }),
            )?;
            let mixer = mixer::spawn_both_mixer(
                far_rx,
                near_rx,
                audio_tx,
                config.enable_aec,
                config.comfort_noise,
                shutdown,
            );
            Ok((vec![system, mic], Some(mixer)))
        },
        source => {
            let handle = start_capture(
                source.clone(),
                Box::new(CaptureAudioCallback {
                    sink: AudioSink::Mixed(audio_tx),
                    paused,
                    drop_counter,
                }),
            )?;
            Ok((vec![handle], None))
        },
    }
}

struct PipelineTranscriptionCallback {
    segments: Arc<Mutex<Vec<TranscriptionSegment>>>,
    transcript: Arc<Mutex<Box<dyn TranscriptFormatter>>>,
    metrics: Arc<PipelineMetrics>,
    /// Live feed for CLI/GUI progress (partials + finals).
    segment_tx: broadcast::Sender<TranscriptionSegment>,
}

impl TranscriptionCallback for PipelineTranscriptionCallback {
    fn on_segment(
        &self,
        segment: TranscriptionSegment,
    ) {
        // Forward partials for live preview (`current_output`); finals also
        // update the durable segment list and metrics.
        if let Ok(mut transcript) = self.transcript.lock() {
            transcript.write_segment(&segment);
        }
        let _ = self.segment_tx.send(segment.clone());
        if segment.is_final {
            if let Ok(mut segments) = self.segments.lock() {
                segments.push(segment);
            }
            self.metrics.record_segment();
        }
    }

    fn on_error(
        &self,
        error: String,
    ) {
        log::error!("transcription error: {error}");
    }
}

impl RecordingPipeline {
    /// Validates configuration, starts native capture, and spawns the consumer.
    ///
    /// # Errors
    ///
    /// Returns [`PipelineError`] when validation, permissions, or setup fails.
    pub async fn start(config: PipelineConfig) -> Result<Self, PipelineError> {
        validate_config(&config)?;
        check_permissions(&config.source)?;
        let audio_format = config.audio_format.clone();
        check_disk_space(
            &config.output_path,
            &audio_format,
            config.estimated_duration_hours,
        )?;

        if config.output_path.exists() {
            return Err(RecordingError::OutputExists {
                path: config
                    .output_path
                    .to_str()
                    .unwrap_or("<invalid utf-8>")
                    .to_owned(),
            }
            .into());
        }

        let comments = OggComments::for_session(&config.source, &config.locale);
        let encoder = create_encoder(&audio_format, Some(&comments))?;

        // Start capture before any `.await` so ScreenCaptureKit / CoreGraphics
        // run on the runtime's initial (CLI main) thread.
        let shutdown = Arc::new(AtomicBool::new(false));
        let paused = Arc::new(AtomicBool::new(false));
        let drop_counter = Arc::new(AtomicU64::new(0));
        let metrics = PipelineMetrics::new();
        let segments = Arc::new(Mutex::new(Vec::new()));
        let transcript_meta = TranscriptMeta::for_session(&config.source, &config.locale);
        let transcript_fmt = create_formatter(config.transcript_format, &transcript_meta);
        let transcript = Arc::new(Mutex::new(transcript_fmt));

        let (segment_tx, _) = broadcast::channel(256);
        let (transcription_handle, speech) = open_transcription(
            &config,
            &segments,
            &transcript,
            &metrics,
            segment_tx.clone(),
        )?;

        let (audio_tx, audio_rx) = broadcast::channel(64);
        let (capture_handles, mixer_task) = start_captures(
            &config,
            audio_tx,
            Arc::clone(&paused),
            Arc::clone(&drop_counter),
            Arc::clone(&shutdown),
        )?;

        let file_writer = FileWriter::create(&config.output_path).await?;

        let encoder = Arc::new(Mutex::new(encoder));
        let file_writer = Arc::new(AsyncMutex::new(file_writer));
        let (progress_tx, _) = broadcast::channel(32);
        let start_time = Instant::now();
        let started_at = Arc::new(Mutex::new(start_time));
        let bytes_written = Arc::new(AtomicU64::new(0));
        let monitor = if config.monitor {
            start_session_monitor()
        } else {
            None
        };

        let consumer_ctx = ConsumerContext {
            encoder: Arc::clone(&encoder),
            speech,
            writer: Arc::clone(&file_writer),
            metrics: Arc::clone(&metrics),
            shutdown: Arc::clone(&shutdown),
            paused: Arc::clone(&paused),
            progress_tx: progress_tx.clone(),
            started_at: Arc::clone(&started_at),
            bytes_written: Arc::clone(&bytes_written),
            monitor: monitor.clone(),
        };
        // Use the original receiver so chunks that arrive while the output
        // file is created are not discarded with a leftover `_audio_rx`.
        let consumer_task = spawn_consumer(audio_rx, consumer_ctx);

        Ok(Self {
            config,
            state: PipelineState::Recording,
            encoder,
            transcript_fmt: transcript,
            file_writer,
            capture_handles,
            mixer_task,
            transcription_handle,
            consumer_task: Some(consumer_task),
            shutdown,
            paused,
            drop_counter,
            metrics,
            segments,
            progress_tx,
            segment_tx,
            started_at,
            bytes_written,
            monitor,
        })
    }

    /// Pauses audio production while keeping the native tap alive.
    pub fn pause(&mut self) {
        if matches!(self.state, PipelineState::Recording) {
            self.paused.store(true, Ordering::Relaxed);
            self.state = PipelineState::Paused {
                elapsed_before_pause: self.elapsed(),
            };
            self.publish_status(RecordingState::Paused, 0.0, 0.0);
        }
    }

    /// Resumes recording after a pause.
    pub fn resume(&mut self) {
        if let PipelineState::Paused {
            elapsed_before_pause,
        } = self.state
        {
            self.paused.store(false, Ordering::Relaxed);
            let start_time = Instant::now()
                .checked_sub(elapsed_before_pause)
                .unwrap_or_else(Instant::now);
            if let Ok(mut origin) = self.started_at.lock() {
                *origin = start_time;
            }
            self.state = PipelineState::Recording;
            self.publish_status(RecordingState::Recording, 0.0, 0.0);
        }
    }

    fn elapsed(&self) -> Duration {
        self.started_at
            .lock()
            .map_or(Duration::ZERO, |origin| origin.elapsed())
    }

    /// Returns whether the pipeline is paused.
    #[must_use]
    pub const fn is_paused(&self) -> bool {
        matches!(self.state, PipelineState::Paused { .. })
    }

    /// Current lifecycle state.
    #[cfg(test)]
    #[must_use]
    pub(crate) const fn state(&self) -> PipelineState {
        self.state
    }

    /// Runtime metrics snapshot.
    #[must_use]
    pub fn metrics(&self) -> PipelineMetricsSnapshot {
        self.metrics.snapshot()
    }

    /// Returns the primary capture handle when a session is active.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn capture_handle(&self) -> Option<&Arc<CaptureHandle>> {
        self.capture_handles.first()
    }

    /// Transcription handle for test injection of segments.
    #[cfg(test)]
    #[must_use]
    pub(crate) const fn transcription_handle(&self) -> Option<&Arc<TranscriptionHandle>> {
        self.transcription_handle.as_ref()
    }

    /// Subscribes to recording progress for CLI/GUI surfaces.
    ///
    /// Delivery is best-effort over a bounded broadcast channel: a slow
    /// subscriber may observe [`broadcast::error::RecvError::Lagged`] and miss
    /// intermediate meter updates. Lifecycle transitions (`Paused`, `Stopping`,
    /// `Stopped`) are emitted explicitly from pause/resume/stop.
    #[must_use]
    pub fn subscribe_progress(&self) -> broadcast::Receiver<RecordingStatus> {
        self.progress_tx.subscribe()
    }

    /// Subscribes to live transcription segments (partials and finals).
    ///
    /// Delivery is best-effort over a bounded broadcast channel. Unlike
    /// [`Self::subscribe_progress`] (where a missed meter tick is just a stale
    /// snapshot), a lagged subscriber **permanently skips** those segment
    /// events — finals are not resent. Handle
    /// [`broadcast::error::RecvError::Lagged`] accordingly.
    ///
    /// When transcription is disabled, no events are sent; `recv` stays pending
    /// until the pipeline (and this sender) are dropped — use `select!` rather
    /// than awaiting this channel alone.
    #[must_use]
    pub fn subscribe_segments(&self) -> broadcast::Receiver<TranscriptionSegment> {
        self.segment_tx.subscribe()
    }

    fn publish_status(
        &self,
        state: RecordingState,
        level_left: f32,
        level_right: f32,
    ) {
        let elapsed_ms = elapsed_ms(&self.started_at);
        let _ = self.progress_tx.send(RecordingStatus {
            elapsed_ms,
            bytes_written: self.bytes_written.load(Ordering::Relaxed),
            level_left,
            level_right,
            state,
        });
    }
}

fn elapsed_ms(started_at: &Mutex<Instant>) -> u64 {
    started_at.lock().map_or(0, |origin| {
        u64::try_from(origin.elapsed().as_millis()).unwrap_or(u64::MAX)
    })
}

fn validate_config(config: &PipelineConfig) -> Result<(), PipelineError> {
    validate_capture_source(&config.source)?;
    if config.transcribe {
        validate_locale(&config.locale)?;
    } else if config.transcript_output_path.is_some() {
        return Err(RecordingError::ConfigError {
            msg: "transcript_output_path requires transcribe=true".to_owned(),
        }
        .into());
    }
    let output = config
        .output_path
        .to_str()
        .ok_or_else(|| RecordingError::ConfigError {
            msg: "output path is not valid UTF-8".to_owned(),
        })?;
    validate_output_path(output)?;
    if let Some(path) = &config.transcript_output_path
        && path.exists()
    {
        return Err(RecordingError::OutputExists {
            path: path.to_str().unwrap_or("<invalid utf-8>").to_owned(),
        }
        .into());
    }
    Ok(())
}

fn open_transcription(
    config: &PipelineConfig,
    segments: &Arc<Mutex<Vec<TranscriptionSegment>>>,
    transcript: &Arc<Mutex<Box<dyn TranscriptFormatter>>>,
    metrics: &Arc<PipelineMetrics>,
    segment_tx: broadcast::Sender<TranscriptionSegment>,
) -> Result<TranscriptionSetup, PipelineError> {
    if !config.transcribe {
        return Ok((None, None));
    }
    let transcription_callback = PipelineTranscriptionCallback {
        segments: Arc::clone(segments),
        transcript: Arc::clone(transcript),
        metrics: Arc::clone(metrics),
        segment_tx,
    };
    let handle = start_transcription(
        config.locale.clone(),
        config.speech_engine,
        Box::new(transcription_callback),
    )?;
    let feeder = Arc::new(consumer::TranscriptionFeeder::new(Arc::clone(&handle)));
    Ok((Some(handle), Some(feeder)))
}

type TranscriptionSetup = (
    Option<Arc<TranscriptionHandle>>,
    Option<Arc<dyn SpeechFeeder>>,
);

fn check_permissions(source: &AudioSourceConfig) -> Result<(), PipelineError> {
    for permission in required_permissions(source) {
        let status = check_permission(permission);
        if status != PermissionStatus::Authorized {
            let name = permission_name(permission);
            return Err(PipelineError::PermissionDenied(name.to_owned()));
        }
    }
    Ok(())
}

fn required_permissions(source: &AudioSourceConfig) -> Vec<Permission> {
    match source {
        AudioSourceConfig::Microphone => vec![Permission::Microphone],
        AudioSourceConfig::AppAudio { .. } | AudioSourceConfig::PidAudio { .. } => {
            vec![Permission::ScreenRecording]
        },
        AudioSourceConfig::Both { .. } => {
            vec![Permission::Microphone, Permission::ScreenRecording]
        },
    }
}

const fn permission_name(permission: Permission) -> &'static str {
    match permission {
        Permission::Microphone => "microphone",
        Permission::ScreenRecording => "screen recording",
        Permission::Accessibility => "accessibility",
    }
}

#[cfg(test)]
mod tests {
    use koe_ffi::{Permission, PermissionStatus};

    use super::test_support::{install_provider, test_config, unique_path};
    use super::*;

    #[tokio::test]
    async fn start_stop_with_authorized_permissions() {
        let _guard =
            install_provider(vec![(Permission::Microphone, PermissionStatus::Authorized)]).await;
        let output = unique_path("start-stop");

        let mut pipeline = RecordingPipeline::start(test_config(&output))
            .await
            .expect("start");

        if let Some(handle) = pipeline.capture_handle() {
            handle.deliver_audio(vec![0.1, -0.1, 0.2, -0.2], 10);
            handle.deliver_audio(vec![0.3, -0.3], 20);
        }

        let summary = pipeline.stop().await.expect("stop");
        assert!(summary.bytes_written > 0);
        assert_eq!(summary.dropped_audio_frames, 0);

        let _ = std::fs::remove_file(output);
    }

    #[tokio::test]
    async fn pause_resume_cycle() {
        let _guard =
            install_provider(vec![(Permission::Microphone, PermissionStatus::Authorized)]).await;
        let output = unique_path("pause");

        let mut pipeline = RecordingPipeline::start(test_config(&output))
            .await
            .expect("start");
        assert!(!pipeline.is_paused());

        pipeline.pause();
        assert!(pipeline.is_paused());

        if let Some(handle) = pipeline.capture_handle() {
            handle.deliver_audio(vec![1.0, -1.0], 30);
        }

        pipeline.resume();
        assert!(!pipeline.is_paused());

        if let Some(handle) = pipeline.capture_handle() {
            handle.deliver_audio(vec![0.5, -0.5], 40);
        }

        let _ = pipeline.stop().await.expect("stop");
        let _ = std::fs::remove_file(output);
    }

    #[tokio::test]
    async fn progress_emits_lifecycle_states() {
        let _guard =
            install_provider(vec![(Permission::Microphone, PermissionStatus::Authorized)]).await;
        let output = unique_path("progress");

        let mut pipeline = RecordingPipeline::start(test_config(&output))
            .await
            .expect("start");
        let mut progress = pipeline.subscribe_progress();

        pipeline.pause();
        let paused = progress.try_recv().expect("paused status");
        assert_eq!(paused.state, RecordingState::Paused);

        pipeline.resume();
        let resumed = progress.try_recv().expect("recording status");
        assert_eq!(resumed.state, RecordingState::Recording);

        let _ = pipeline.stop().await.expect("stop");

        let mut saw_stopping = false;
        let mut saw_stopped = false;
        while let Ok(status) = progress.try_recv() {
            saw_stopping |= status.state == RecordingState::Stopping;
            saw_stopped |= status.state == RecordingState::Stopped;
        }
        assert!(saw_stopping);
        assert!(saw_stopped);

        let _ = std::fs::remove_file(output);
    }

    #[tokio::test]
    async fn start_without_transcription() {
        let _guard =
            install_provider(vec![(Permission::Microphone, PermissionStatus::Authorized)]).await;
        let output = unique_path("no-asr");

        let mut config = test_config(&output);
        config.transcribe = false;
        let mut pipeline = RecordingPipeline::start(config).await.expect("start");
        assert!(pipeline.transcription_handle().is_none());

        if let Some(handle) = pipeline.capture_handle() {
            handle.deliver_audio(vec![0.1, -0.1], 10);
        }

        let summary = pipeline.stop().await.expect("stop");
        assert!(summary.bytes_written > 0);
        assert_eq!(summary.transcript_segment_count, 0);

        let _ = std::fs::remove_file(output);
    }

    #[tokio::test]
    async fn start_with_denied_permission_fails() {
        let _guard =
            install_provider(vec![(Permission::Microphone, PermissionStatus::Denied)]).await;
        let output = unique_path("denied");
        let Err(err) = RecordingPipeline::start(test_config(&output)).await else {
            panic!("permission denied");
        };
        assert!(matches!(err, PipelineError::PermissionDenied(_)));
    }

    #[tokio::test]
    async fn start_with_insufficient_disk_space_fails() {
        let _guard =
            install_provider(vec![(Permission::Microphone, PermissionStatus::Authorized)]).await;
        let output = unique_path("disk");
        let mut config = test_config(&output);
        config.estimated_duration_hours = Some(1_000_000.0);
        config.audio_format = OutputFormat::Ogg { quality: 0.4 };

        let Err(err) = RecordingPipeline::start(config).await else {
            panic!("disk full");
        };
        assert!(matches!(
            err,
            PipelineError::Recording(RecordingError::InsufficientDiskSpace { .. })
        ));
    }
}

//! Graceful and force shutdown for [`super::RecordingPipeline`].
//!
//! Sequence (graceful):
//! 1. Set shutdown flag
//! 2. Stop native capture
//! 3. Drain consumer (budget: [`SHUTDOWN_BUDGET`])
//! 4. Stop audio monitor (`AudioQueue` teardown)
//! 5. Finalize speech analyzer
//! 6. Finalize encoder + flush audio
//! 7. Write transcript
//! 8. Emit [`StopResult`]
//!
//! Force mode skips the drain / ASR finalize wait but still finalizes the
//! encoder so on-disk audio containers stay valid.
//!
//! `force_stop` is choose-up-front (or call instead of `stop`), not an
//! in-flight escalate while `stop().await` holds `&mut self`. CLI double
//! SIGINT (task 29) should prefer `force_stop` over `process::exit`.

use std::ops::Deref;
use std::sync::atomic::Ordering;
use std::time::Duration;

use koe_ffi::{RecordingState, RecordingSummary, finalize_transcription, stop_capture};

use super::{PipelineError, PipelineState, RecordingPipeline};

/// Maximum time spent waiting for the consumer to drain during graceful stop.
///
/// Encoder finalize, disk flush, and transcript write are outside this budget
/// (typically ≪ 100 ms). Spec ceiling for the full sequence is 2 s.
pub const SHUTDOWN_BUDGET: Duration = Duration::from_secs(2);

/// Brief wait before aborting the consumer on [`ShutdownMode::Force`].
pub const FORCE_JOIN_BUDGET: Duration = Duration::from_millis(50);

/// How [`RecordingPipeline::stop_with`] finalizes the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ShutdownMode {
    /// Drain remaining audio, finalize ASR, then finalize encoder / transcript.
    #[default]
    Graceful,
    /// Abort the consumer quickly; skip ASR finalize. Encoder is still
    /// finalized so container files are not left corrupt.
    Force,
}

/// Outcome of [`RecordingPipeline::stop`] / [`RecordingPipeline::force_stop`].
///
/// Derefs to [`RecordingSummary`] so existing `result.bytes_written` style
/// access keeps working.
#[derive(Debug, Clone)]
pub struct StopResult {
    /// Recording summary for CLI/GUI surfaces.
    pub summary: RecordingSummary,
    /// `true` when the consumer exited cleanly after draining.
    ///
    /// `false` when the consumer was aborted (force / drain timeout) or
    /// returned an error. Encoder finalize still ran, so audio files should
    /// remain readable; some trailing audio or partial transcript may be lost.
    pub consumer_drained: bool,
}

impl Deref for StopResult {
    type Target = RecordingSummary;

    fn deref(&self) -> &Self::Target {
        &self.summary
    }
}

#[derive(Debug)]
enum DrainOutcome {
    Drained,
    Aborted,
    Failed(PipelineError),
}

impl RecordingPipeline {
    /// Stops native capture (`ScreenCaptureKit` / Process Tap / mic) and the
    /// both-source mixer.
    ///
    /// Call from the process main thread when `ScreenCaptureKit` is active so the
    /// `AppKit` main runloop can pump `stopCapture` completion handlers. Safe to
    /// call multiple times; [`Self::stop`] skips capture teardown when handles
    /// are already gone.
    pub fn stop_native_captures(&mut self) {
        if matches!(self.state, PipelineState::Stopped) {
            return;
        }

        if !self.shutdown.load(Ordering::Acquire) {
            self.shutdown.store(true, Ordering::Release);
            self.paused.store(false, Ordering::Release);
            self.publish_status(RecordingState::Stopping, 0.0, 0.0);
        }

        // Dropping capture handles drops producers so the consumer / mixer
        // unblock even if no more audio arrives.
        //
        // ScreenCaptureKit teardown must run on the main thread with the main
        // runloop pumped (see `screen_audio::ScreenAudioSession::stop`).
        for handle in self.capture_handles.drain(..) {
            stop_capture(handle);
        }
        if let Some(mixer) = self.mixer_task.take() {
            mixer.abort();
        }
    }

    /// Stops capture, drains remaining audio, finalizes outputs, and returns a summary.
    ///
    /// Waits up to [`SHUTDOWN_BUDGET`] for the consumer to drain. On timeout the
    /// consumer is aborted and **joined** before encoder finalize so no writer
    /// races the trailer. [`StopResult::consumer_drained`] is `false` in that case.
    ///
    /// # Errors
    ///
    /// Returns [`PipelineError`] when the pipeline is not running or
    /// finalization (encoder / disk / transcript) fails.
    pub async fn stop(&mut self) -> Result<StopResult, PipelineError> {
        self.stop_with(ShutdownMode::Graceful).await
    }

    /// Force-stops without waiting for a full drain / ASR flush.
    ///
    /// Partial transcript segments may be lost. Encoded audio is still
    /// finalized and flushed so files remain readable.
    ///
    /// This is not an in-flight escalate for a concurrent `stop().await`
    /// (both need `&mut self`). Call this instead of `stop` when the user
    /// requests an immediate exit.
    ///
    /// # Errors
    ///
    /// Returns [`PipelineError`] when the pipeline is not running or
    /// finalization fails.
    pub async fn force_stop(&mut self) -> Result<StopResult, PipelineError> {
        self.stop_with(ShutdownMode::Force).await
    }

    /// Stops with an explicit [`ShutdownMode`]. Prefer [`Self::stop`] /
    /// [`Self::force_stop`] at call sites.
    ///
    /// # Errors
    ///
    /// Returns [`PipelineError`] when the pipeline is not running or a
    /// finalization step fails.
    pub(crate) async fn stop_with(
        &mut self,
        mode: ShutdownMode,
    ) -> Result<StopResult, PipelineError> {
        if matches!(self.state, PipelineState::Stopped) {
            return Err(PipelineError::InvalidState(
                "pipeline already stopped".to_owned(),
            ));
        }

        self.stop_native_captures();

        let drain = self.join_consumer(mode).await;
        let consumer_drained = matches!(drain, DrainOutcome::Drained);
        if let DrainOutcome::Failed(err) = &drain {
            log::error!("consumer failed during shutdown: {err}");
        }

        // Tear down AudioQueue after the consumer has drained so late writes
        // during drain still reach the device, then release the output.
        if let Some(monitor) = &self.monitor {
            monitor.stop();
        }

        let duration_sec = match self.state {
            PipelineState::Recording => self.elapsed().as_secs_f64(),
            PipelineState::Paused {
                elapsed_before_pause,
            } => elapsed_before_pause.as_secs_f64(),
            PipelineState::Stopped => 0.0,
        };

        // Capture/consumer are gone; mark Stopped even if finalize fails so
        // the state machine does not claim we are still recording.
        let finalize = self.finalize_outputs(mode).await;
        self.state = PipelineState::Stopped;
        self.publish_status(RecordingState::Stopped, 0.0, 0.0);

        let bytes_written = finalize?;
        let segment_count = self
            .segments
            .lock()
            .map_err(|_| PipelineError::InvalidState("segments lock poisoned".to_owned()))?
            .len();

        Ok(StopResult {
            summary: RecordingSummary {
                duration_sec,
                bytes_written,
                transcript_segment_count: u64::try_from(segment_count).unwrap_or(u64::MAX),
                dropped_audio_frames: self.drop_counter.load(Ordering::Relaxed),
                format: self.config.audio_format.clone(),
            },
            consumer_drained,
        })
    }

    /// ASR finalize (graceful only) + encoder trailer + transcript write.
    async fn finalize_outputs(
        &mut self,
        mode: ShutdownMode,
    ) -> Result<u64, PipelineError> {
        if mode == ShutdownMode::Graceful {
            if let Some(handle) = self.transcription_handle.take() {
                finalize_transcription(handle);
            }
        } else {
            // Force: drop without finalize — partials may be lost.
            self.transcription_handle.take();
        }

        // Always finalize the encoder after the consumer has fully stopped
        // (drained, failed, or aborted+joined) so trailers cannot race writes.
        let trailer = {
            let mut encoder = self
                .encoder
                .lock()
                .map_err(|_| PipelineError::InvalidState("encoder lock poisoned".to_owned()))?;
            encoder.finalize()?
        };

        let bytes_written = {
            let mut writer = self.file_writer.lock().await;
            if !trailer.is_empty() {
                let written = u64::try_from(trailer.len()).unwrap_or(u64::MAX);
                writer.write(&trailer).await?;
                self.bytes_written.fetch_add(written, Ordering::Relaxed);
            }
            writer.flush().await?;
            writer.bytes_written()
        };
        self.bytes_written.store(bytes_written, Ordering::Relaxed);

        if let Some(transcript_path) = &self.config.transcript_output_path {
            let body = {
                let transcript = self.transcript_fmt.lock().map_err(|_| {
                    PipelineError::InvalidState("transcript lock poisoned".to_owned())
                })?;
                // Trait-object path: `finalize` requires `Sized`; committed
                // output excludes in-flight partials (same as `finalize`).
                transcript.committed_output()
            };
            tokio::fs::write(transcript_path, body).await?;
        }

        Ok(bytes_written)
    }

    /// Waits for the consumer within the mode budget, then aborts and **joins**
    /// on timeout so finalize cannot race in-flight encode/write.
    async fn join_consumer(
        &mut self,
        mode: ShutdownMode,
    ) -> DrainOutcome {
        let Some(mut task) = self.consumer_task.take() else {
            return DrainOutcome::Drained;
        };

        let budget = match mode {
            ShutdownMode::Graceful => SHUTDOWN_BUDGET,
            ShutdownMode::Force => FORCE_JOIN_BUDGET,
        };

        tokio::select! {
            biased;
            result = &mut task => Self::map_join_result(result),
            () = tokio::time::sleep(budget) => {
                task.abort();
                if mode == ShutdownMode::Graceful {
                    log::warn!(
                        "consumer drain exceeded {budget:?}; aborting and continuing finalize"
                    );
                }
                // Join after abort so spawn_blocking encode/write cannot
                // append past the container trailer.
                Self::map_join_result(task.await)
            },
        }
    }

    fn map_join_result(
        result: Result<Result<(), PipelineError>, tokio::task::JoinError>
    ) -> DrainOutcome {
        match result {
            Ok(Ok(())) => DrainOutcome::Drained,
            Ok(Err(err)) => DrainOutcome::Failed(err),
            Err(err) if err.is_cancelled() => DrainOutcome::Aborted,
            Err(err) => DrainOutcome::Failed(PipelineError::InvalidState(format!(
                "consumer task join failed: {err}"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::{Arc, Mutex};
    use std::time::Instant;

    use koe_ffi::{OutputFormat, RecordingState, TranscriptionSegment};

    use super::*;
    use crate::codec::{AudioEncoder, CodecError};
    use crate::pipeline::test_support::{install_authorized_mic, test_config, unique_path};

    fn assert_valid_ogg(path: &Path) {
        let bytes = std::fs::read(path).expect("read ogg");
        assert!(bytes.len() >= 56, "OGG too small: {} bytes", bytes.len());
        assert_eq!(&bytes[0..4], b"OggS");
        assert!(
            String::from_utf8_lossy(&bytes).contains("Opus"),
            "expected an Opus bitstream"
        );
    }

    #[tokio::test]
    async fn stop_immediately_is_clean() {
        let _guard = install_authorized_mic().await;
        let output = unique_path("immediate");
        let mut pipeline = RecordingPipeline::start(test_config(&output))
            .await
            .expect("start");

        let started = Instant::now();
        let result = pipeline.stop().await.expect("stop");
        assert!(started.elapsed() < SHUTDOWN_BUDGET);
        assert!(result.consumer_drained);
        assert!(matches!(pipeline.state(), PipelineState::Stopped));
        assert!(matches!(
            result.format,
            OutputFormat::Ogg { bitrate_bps: None }
        ));
        assert_valid_ogg(&output);
        let _ = std::fs::remove_file(output);
    }

    #[tokio::test]
    async fn stop_with_monitor_enabled_is_clean() {
        let _guard = install_authorized_mic().await;
        let output = unique_path("monitor");
        let mut config = test_config(&output);
        config.monitor = true;
        let mut pipeline = RecordingPipeline::start(config).await.expect("start");

        let started = Instant::now();
        let summary = pipeline.stop().await.expect("stop");
        assert!(started.elapsed() < SHUTDOWN_BUDGET);
        assert!(matches!(pipeline.state(), PipelineState::Stopped));
        assert_valid_ogg(&output);
        assert!(summary.bytes_written > 0);
        let _ = std::fs::remove_file(output);
    }

    #[tokio::test]
    async fn stop_after_audio_processes_all_frames() {
        let _guard = install_authorized_mic().await;
        let output = unique_path("drain");
        let mut pipeline = RecordingPipeline::start(test_config(&output))
            .await
            .expect("start");

        // 3 chunks × stereo pairs → frame counts 2 + 1 + 2 = 5.
        let chunks = [
            vec![0.1, -0.1, 0.2, -0.2],
            vec![0.3, -0.3],
            vec![0.4, -0.4, 0.5, -0.5],
        ];
        let expected_frames: u64 = chunks.iter().map(|c| (c.len() / 2) as u64).sum();

        if let Some(handle) = pipeline.capture_handle() {
            for (i, samples) in chunks.iter().enumerate() {
                handle.deliver_audio(samples.clone(), (i as u64 + 1) * 20);
            }
        }

        // Stop drains the broadcast channel; no wall-clock race required.
        let result = pipeline.stop().await.expect("stop");
        assert!(result.consumer_drained);
        assert!(result.bytes_written > 0);
        assert_eq!(result.dropped_audio_frames, 0);
        assert_eq!(
            pipeline.metrics().total_frames_processed,
            expected_frames,
            "all fed frames must be processed before exit"
        );
        assert_valid_ogg(&output);
        let _ = std::fs::remove_file(output);
    }

    #[tokio::test]
    async fn force_stop_does_not_corrupt_ogg() {
        let _guard = install_authorized_mic().await;
        let output = unique_path("force");
        let mut pipeline = RecordingPipeline::start(test_config(&output))
            .await
            .expect("start");

        if let Some(handle) = pipeline.capture_handle() {
            for i in 0..16 {
                handle.deliver_audio(vec![0.1, -0.1, 0.2, -0.2], i * 20);
            }
        }

        let started = Instant::now();
        let result = pipeline.force_stop().await.expect("force_stop");
        assert!(started.elapsed() < SHUTDOWN_BUDGET);
        assert!(result.bytes_written > 0);
        assert_valid_ogg(&output);
        let _ = std::fs::remove_file(output);
    }

    #[tokio::test]
    async fn pause_then_stop_keeps_pre_pause_segments() {
        let _guard = install_authorized_mic().await;
        let output = unique_path("pause-stop");
        let transcript = output.with_extension("txt");
        let mut config = test_config(&output);
        config.transcript_output_path = Some(transcript.clone());

        let mut pipeline = RecordingPipeline::start(config).await.expect("start");

        if let Some(handle) = pipeline.transcription_handle() {
            handle.deliver_segment(TranscriptionSegment {
                text: "before pause".into(),
                start_ms: 0,
                end_ms: 500,
                is_final: true,
                confidence: 0.9,
            });
        }

        pipeline.pause();
        assert!(pipeline.is_paused());

        if let Some(handle) = pipeline.capture_handle() {
            // Dropped while paused — must not affect transcript.
            handle.deliver_audio(vec![1.0, -1.0], 30);
        }

        let result = pipeline.stop().await.expect("stop");
        assert_eq!(result.transcript_segment_count, 1);

        let body = std::fs::read_to_string(&transcript).expect("transcript");
        assert!(body.contains("before pause"));

        let _ = std::fs::remove_file(output);
        let _ = std::fs::remove_file(transcript);
    }

    #[tokio::test]
    async fn double_stop_is_rejected() {
        let _guard = install_authorized_mic().await;
        let output = unique_path("double");
        let mut pipeline = RecordingPipeline::start(test_config(&output))
            .await
            .expect("start");
        let _ = pipeline.stop().await.expect("first stop");
        let err = pipeline.stop().await.expect_err("second stop");
        assert!(matches!(err, PipelineError::InvalidState(_)));
        let _ = std::fs::remove_file(output);
    }

    #[tokio::test]
    async fn stop_emits_stopping_then_stopped() {
        let _guard = install_authorized_mic().await;
        let output = unique_path("status");
        let mut pipeline = RecordingPipeline::start(test_config(&output))
            .await
            .expect("start");
        let mut progress = pipeline.subscribe_progress();

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

    /// Slow encoder that holds the mutex across a sleep inside `spawn_blocking`.
    struct SlowFinalizeEncoder {
        encode_delay: Duration,
        pcm_bytes: Arc<Mutex<Vec<u8>>>,
    }

    impl AudioEncoder for SlowFinalizeEncoder {
        fn encode(
            &mut self,
            pcm: &[f32],
        ) -> Result<Vec<u8>, CodecError> {
            std::thread::sleep(self.encode_delay);
            let mut bytes = Vec::with_capacity(pcm.len() * 4);
            for sample in pcm {
                bytes.extend_from_slice(&sample.to_le_bytes());
            }
            self.pcm_bytes
                .lock()
                .expect("lock")
                .extend_from_slice(&bytes);
            Ok(bytes)
        }

        fn finalize(&mut self) -> Result<Vec<u8>, CodecError> {
            Ok(b"TRAILER".to_vec())
        }
    }

    #[tokio::test]
    async fn force_stop_joins_before_finalize_no_post_trailer_writes() {
        use std::sync::atomic::{AtomicBool, AtomicU64, Ordering as AtomicOrdering};
        use std::time::Instant as StdInstant;

        use tokio::sync::{Mutex as AsyncMutex, broadcast};

        use crate::pipeline::RecordingStatus;
        use crate::pipeline::chunk::AudioChunk;
        use crate::pipeline::consumer::{ConsumerContext, SpeechFeeder, spawn_consumer};
        use crate::pipeline::file_writer::FileWriter;
        use crate::pipeline::metrics::PipelineMetrics;

        struct NoopSpeech;
        impl SpeechFeeder for NoopSpeech {
            fn feed_audio(
                &self,
                _pcm: Vec<f32>,
            ) {
            }
        }

        let path = unique_path("race");
        let writer = Arc::new(AsyncMutex::new(
            FileWriter::create(&path).await.expect("writer"),
        ));
        let pcm_bytes = Arc::new(Mutex::new(Vec::new()));
        let encoder: Arc<Mutex<Box<dyn AudioEncoder>>> =
            Arc::new(Mutex::new(Box::new(SlowFinalizeEncoder {
                encode_delay: Duration::from_millis(80),
                pcm_bytes: Arc::clone(&pcm_bytes),
            })));

        let shutdown = Arc::new(AtomicBool::new(false));
        let (progress_tx, _) = broadcast::channel::<RecordingStatus>(8);
        let (tx, rx) = broadcast::channel(64);
        let ctx = ConsumerContext {
            encoder: Arc::clone(&encoder),
            speech: Some(Arc::new(NoopSpeech)),
            writer: Arc::clone(&writer),
            metrics: PipelineMetrics::new(),
            shutdown: Arc::clone(&shutdown),
            paused: Arc::new(AtomicBool::new(false)),
            progress_tx,
            started_at: Arc::new(Mutex::new(StdInstant::now())),
            bytes_written: Arc::new(AtomicU64::new(0)),
            monitor: None,
        };
        let task = spawn_consumer(rx, ctx);

        // Queue work that will still be encoding when we abort.
        for i in 0..4 {
            tx.send(AudioChunk::new(vec![0.1, -0.1], i)).expect("send");
        }
        drop(tx);
        shutdown.store(true, AtomicOrdering::Release);

        // Mirror join_consumer Force path: brief wait, abort, join, then finalize.
        let mut task = task;
        tokio::select! {
            biased;
            _ = &mut task => {},
            () = tokio::time::sleep(FORCE_JOIN_BUDGET) => {
                task.abort();
                let _ = task.await;
            },
        }

        let trailer = {
            let mut enc = encoder.lock().expect("encoder");
            enc.finalize().expect("finalize")
        };
        {
            let mut w = writer.lock().await;
            w.write(&trailer).await.expect("write trailer");
            w.flush().await.expect("flush");
            drop(w);
        }

        let bytes = std::fs::read(&path).expect("read");
        let trailer_pos = bytes
            .windows(7)
            .rposition(|w| w == b"TRAILER")
            .expect("trailer present");
        assert_eq!(
            trailer_pos + 7,
            bytes.len(),
            "no encode writes may follow the trailer"
        );

        let _ = std::fs::remove_file(path);
    }
}

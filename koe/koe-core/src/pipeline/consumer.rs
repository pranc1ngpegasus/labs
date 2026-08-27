//! Async consumer loop draining the audio broadcast channel.
//!
//! Flow: broadcast → monitor (optional) → encode (`spawn_blocking`) → async disk
//! write → ASR feed → progress.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use koe_ffi::{RecordingState, RecordingStatus, TranscriptionHandle};
use tokio::sync::{Mutex as AsyncMutex, broadcast};
use tokio::task::JoinHandle;

use crate::codec::AudioEncoder;

use super::PipelineError;
use super::chunk::AudioChunk;
use super::file_writer::FileWriter;
use super::metrics::PipelineMetrics;
use super::monitor::AudioMonitor;

/// Feeds canonical PCM into the speech analyzer (non-blocking on the Rust side).
pub trait SpeechFeeder: Send + Sync {
    /// Accepts one interleaved stereo chunk for recognition.
    ///
    /// Implementations must not block: the consumer awaits this call on its
    /// critical path between encode/write and the next chunk.
    fn feed_audio(
        &self,
        pcm: Vec<f32>,
    );
}

/// Default [`SpeechFeeder`] that forwards into the FFI transcription handle.
pub struct TranscriptionFeeder {
    handle: Arc<TranscriptionHandle>,
}

impl TranscriptionFeeder {
    /// Wraps an FFI transcription session.
    #[must_use]
    pub const fn new(handle: Arc<TranscriptionHandle>) -> Self {
        Self { handle }
    }
}

impl SpeechFeeder for TranscriptionFeeder {
    fn feed_audio(
        &self,
        pcm: Vec<f32>,
    ) {
        koe_ffi::feed_transcription_audio(Arc::clone(&self.handle), pcm);
    }
}

/// Shared state passed into the consumer task.
pub struct ConsumerContext {
    pub encoder: Arc<Mutex<Box<dyn AudioEncoder>>>,
    pub speech: Option<Arc<dyn SpeechFeeder>>,
    pub writer: Arc<AsyncMutex<FileWriter>>,
    pub metrics: Arc<PipelineMetrics>,
    pub shutdown: Arc<AtomicBool>,
    pub paused: Arc<AtomicBool>,
    pub progress_tx: broadcast::Sender<RecordingStatus>,
    /// Pause-aware session origin shared with [`super::RecordingPipeline`].
    pub started_at: Arc<Mutex<Instant>>,
    /// Running total of encoded bytes written (avoids re-locking the writer).
    pub bytes_written: Arc<AtomicU64>,
    /// Clean-audio sink for live monitoring (`None` when disabled).
    pub monitor: Option<Arc<dyn AudioMonitor>>,
}

/// Spawns the background consumer that encodes audio and feeds transcription.
#[must_use]
pub fn spawn_consumer(
    mut rx: broadcast::Receiver<AudioChunk>,
    ctx: ConsumerContext,
) -> JoinHandle<Result<(), PipelineError>> {
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(chunk) => {
                    process_chunk(&ctx, chunk).await?;
                },
                Err(broadcast::error::RecvError::Lagged(dropped)) => {
                    handle_lag(&ctx, dropped);
                },
                Err(broadcast::error::RecvError::Closed) => break,
            }

            if ctx.shutdown.load(Ordering::Acquire) {
                drain_remaining(&mut rx, &ctx).await?;
                break;
            }
        }
        Ok(())
    })
}

fn handle_lag(
    ctx: &ConsumerContext,
    dropped: u64,
) {
    log::warn!("Consumer lagged by {dropped} chunks; audio dropped");
    ctx.metrics.record_drops(dropped);
}

async fn drain_remaining(
    rx: &mut broadcast::Receiver<AudioChunk>,
    ctx: &ConsumerContext,
) -> Result<(), PipelineError> {
    loop {
        match rx.try_recv() {
            Ok(chunk) => process_chunk(ctx, chunk).await?,
            Err(broadcast::error::TryRecvError::Empty | broadcast::error::TryRecvError::Closed) => {
                break;
            },
            Err(broadcast::error::TryRecvError::Lagged(dropped)) => {
                handle_lag(ctx, dropped);
            },
        }
    }
    Ok(())
}

async fn process_chunk(
    ctx: &ConsumerContext,
    chunk: AudioChunk,
) -> Result<(), PipelineError> {
    let frame_count = chunk.frame_count;
    let (level_left, level_right) = peak_levels(&chunk.samples);

    // Feed the monitor before encode so pass-through latency stays ~one block
    // plus the device buffer (spec: ~15–35 ms), independent of codec cost.
    // Failures are non-fatal: monitoring must not abort the recording path.
    if let Some(monitor) = &ctx.monitor
        && let Err(err) = monitor.write(&chunk.samples)
    {
        log::warn!("audio monitor write failed: {err}");
    }

    let encoder_slot = Arc::clone(&ctx.encoder);
    let pcm = chunk.samples;
    let keep_pcm = ctx.speech.is_some();
    let (encoded_bytes, pcm) = tokio::task::spawn_blocking(move || {
        let mut guard = encoder_slot
            .lock()
            .map_err(|_| PipelineError::InvalidState("encoder lock poisoned".to_owned()))?;
        let encoded_bytes = guard.encode(&pcm)?;
        drop(guard);
        let pcm = keep_pcm.then_some(pcm);
        Ok::<_, PipelineError>((encoded_bytes, pcm))
    })
    .await
    .map_err(|err| PipelineError::InvalidState(format!("encode task join failed: {err}")))??;

    if !encoded_bytes.is_empty() {
        let written = u64::try_from(encoded_bytes.len()).unwrap_or(u64::MAX);
        let mut writer = ctx.writer.lock().await;
        writer.write(&encoded_bytes).await?;
        drop(writer);
        ctx.bytes_written.fetch_add(written, Ordering::Relaxed);
    }

    if let (Some(speech), Some(pcm)) = (&ctx.speech, pcm) {
        speech.feed_audio(pcm);
    }

    ctx.metrics
        .record_frames(u64::try_from(frame_count).unwrap_or(0));

    emit_progress(ctx, level_left, level_right);

    Ok(())
}

/// Builds and sends a [`RecordingStatus`] update (best-effort; lag is ignored).
fn emit_progress(
    ctx: &ConsumerContext,
    level_left: f32,
    level_right: f32,
) {
    let state = if ctx.shutdown.load(Ordering::Acquire) {
        RecordingState::Stopping
    } else if ctx.paused.load(Ordering::Relaxed) {
        RecordingState::Paused
    } else {
        RecordingState::Recording
    };

    let elapsed_ms = super::elapsed_ms(&ctx.started_at);

    let _ = ctx.progress_tx.send(RecordingStatus {
        elapsed_ms,
        bytes_written: ctx.bytes_written.load(Ordering::Relaxed),
        level_left,
        level_right,
        state,
    });
}

fn peak_levels(samples: &[f32]) -> (f32, f32) {
    let mut left = 0.0_f32;
    let mut right = 0.0_f32;
    for pair in samples.as_chunks::<2>().0 {
        left = left.max(pair[0].abs());
        right = right.max(pair[1].abs());
    }
    (left, right)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::AtomicUsize;
    use std::time::Duration;

    use tokio::sync::broadcast;

    use super::*;
    use crate::codec::{AudioEncoder, CodecError};
    use crate::pipeline::monitor::RecordingMonitor;

    struct CountingEncoder {
        encode_count: Arc<AtomicUsize>,
        delay: Duration,
    }

    impl AudioEncoder for CountingEncoder {
        fn encode(
            &mut self,
            pcm: &[f32],
        ) -> Result<Vec<u8>, CodecError> {
            if !self.delay.is_zero() {
                std::thread::sleep(self.delay);
            }
            self.encode_count.fetch_add(1, Ordering::Relaxed);
            let mut bytes = Vec::with_capacity(pcm.len() * 4);
            for sample in pcm {
                bytes.extend_from_slice(&sample.to_le_bytes());
            }
            Ok(bytes)
        }

        fn finalize(&mut self) -> Result<Vec<u8>, CodecError> {
            Ok(Vec::new())
        }
    }

    struct CountingSpeech {
        feed_count: Arc<AtomicUsize>,
        samples: Arc<Mutex<Vec<Vec<f32>>>>,
    }

    impl SpeechFeeder for CountingSpeech {
        fn feed_audio(
            &self,
            pcm: Vec<f32>,
        ) {
            self.feed_count.fetch_add(1, Ordering::Relaxed);
            self.samples.lock().expect("lock").push(pcm);
        }
    }

    async fn temp_writer(label: &str) -> (PathBuf, Arc<AsyncMutex<FileWriter>>) {
        let path = std::env::temp_dir().join(format!(
            "koe-consumer-{label}-{}-{}.bin",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let writer = FileWriter::create(&path).await.expect("create writer");
        (path, Arc::new(AsyncMutex::new(writer)))
    }

    fn context(
        codec: Arc<Mutex<Box<dyn AudioEncoder>>>,
        speech: Option<Arc<dyn SpeechFeeder>>,
        writer: Arc<AsyncMutex<FileWriter>>,
        shutdown: Arc<AtomicBool>,
        progress_tx: broadcast::Sender<RecordingStatus>,
        monitor: Option<Arc<dyn AudioMonitor>>,
    ) -> (ConsumerContext, Arc<PipelineMetrics>) {
        let metrics = PipelineMetrics::new();
        let ctx = ConsumerContext {
            encoder: codec,
            speech,
            writer,
            metrics: Arc::clone(&metrics),
            shutdown,
            paused: Arc::new(AtomicBool::new(false)),
            progress_tx,
            started_at: Arc::new(Mutex::new(Instant::now())),
            bytes_written: Arc::new(AtomicU64::new(0)),
            monitor,
        };
        (ctx, metrics)
    }

    #[tokio::test]
    async fn all_chunks_reach_encoder_and_speech() {
        let encode_count = Arc::new(AtomicUsize::new(0));
        let feed_count = Arc::new(AtomicUsize::new(0));
        let fed_samples = Arc::new(Mutex::new(Vec::new()));
        let shutdown = Arc::new(AtomicBool::new(false));
        let (progress_tx, mut progress_rx) = broadcast::channel(32);
        let (path, writer) = temp_writer("all").await;

        let codec: Arc<Mutex<Box<dyn AudioEncoder>>> =
            Arc::new(Mutex::new(Box::new(CountingEncoder {
                encode_count: Arc::clone(&encode_count),
                delay: Duration::ZERO,
            })));
        let speech: Option<Arc<dyn SpeechFeeder>> = Some(Arc::new(CountingSpeech {
            feed_count: Arc::clone(&feed_count),
            samples: Arc::clone(&fed_samples),
        }));

        let (tx, rx) = broadcast::channel(64);
        let (ctx, metrics) = context(
            codec,
            speech,
            Arc::clone(&writer),
            Arc::clone(&shutdown),
            progress_tx,
            None,
        );
        let task = spawn_consumer(rx, ctx);

        let chunks = [
            vec![0.1, -0.1, 0.2, -0.2],
            vec![0.3, -0.3],
            vec![0.5, -0.5, 0.25, -0.25],
        ];
        for (i, samples) in chunks.iter().enumerate() {
            tx.send(AudioChunk::new(samples.clone(), (i as u64 + 1) * 20))
                .expect("send");
        }

        // Allow the consumer to process at "real-time" pacing.
        tokio::time::sleep(Duration::from_millis(50)).await;
        shutdown.store(true, Ordering::Relaxed);
        drop(tx);

        task.await.expect("join").expect("consumer ok");

        assert_eq!(encode_count.load(Ordering::Relaxed), chunks.len());
        assert_eq!(feed_count.load(Ordering::Relaxed), chunks.len());
        assert_eq!(fed_samples.lock().expect("lock").len(), chunks.len());
        assert_eq!(
            metrics.snapshot().total_frames_processed,
            chunks.iter().map(|c| (c.len() / 2) as u64).sum::<u64>()
        );

        let mut progress_count = 0;
        while progress_rx.try_recv().is_ok() {
            progress_count += 1;
        }
        assert!(progress_count >= chunks.len());

        let bytes = writer.lock().await.bytes_written();
        assert!(bytes > 0);

        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn lag_is_recorded_and_consumer_continues() {
        let encode_count = Arc::new(AtomicUsize::new(0));
        let feed_count = Arc::new(AtomicUsize::new(0));
        let shutdown = Arc::new(AtomicBool::new(false));
        let (progress_tx, _) = broadcast::channel(8);
        let (path, writer) = temp_writer("lag").await;

        let codec: Arc<Mutex<Box<dyn AudioEncoder>>> =
            Arc::new(Mutex::new(Box::new(CountingEncoder {
                encode_count: Arc::clone(&encode_count),
                delay: Duration::from_millis(30),
            })));
        let speech: Option<Arc<dyn SpeechFeeder>> = Some(Arc::new(CountingSpeech {
            feed_count: Arc::clone(&feed_count),
            samples: Arc::new(Mutex::new(Vec::new())),
        }));

        // Tiny buffer so a slow consumer must lag.
        let (tx, rx) = broadcast::channel(1);
        let (ctx, metrics) = context(
            codec,
            speech,
            writer,
            Arc::clone(&shutdown),
            progress_tx,
            None,
        );
        let task = spawn_consumer(rx, ctx);

        for i in 0..20 {
            let _ = tx.send(AudioChunk::new(vec![0.1, -0.1], i));
        }

        tokio::time::sleep(Duration::from_millis(200)).await;
        shutdown.store(true, Ordering::Relaxed);
        drop(tx);

        task.await.expect("join").expect("consumer ok");

        assert!(metrics.snapshot().dropped_frames > 0);
        assert!(encode_count.load(Ordering::Relaxed) > 0);
        assert_eq!(
            encode_count.load(Ordering::Relaxed),
            feed_count.load(Ordering::Relaxed)
        );

        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn shutdown_drains_without_panic() {
        let encode_count = Arc::new(AtomicUsize::new(0));
        let feed_count = Arc::new(AtomicUsize::new(0));
        let shutdown = Arc::new(AtomicBool::new(false));
        let (progress_tx, _) = broadcast::channel(8);
        let (path, writer) = temp_writer("drain").await;

        let codec: Arc<Mutex<Box<dyn AudioEncoder>>> =
            Arc::new(Mutex::new(Box::new(CountingEncoder {
                encode_count: Arc::clone(&encode_count),
                delay: Duration::ZERO,
            })));
        let speech: Option<Arc<dyn SpeechFeeder>> = Some(Arc::new(CountingSpeech {
            feed_count: Arc::clone(&feed_count),
            samples: Arc::new(Mutex::new(Vec::new())),
        }));

        let (tx, rx) = broadcast::channel(64);
        let (ctx, _) = context(
            codec,
            speech,
            writer,
            Arc::clone(&shutdown),
            progress_tx,
            None,
        );
        let task = spawn_consumer(rx, ctx);

        for i in 0..8 {
            tx.send(AudioChunk::new(vec![0.2, -0.2], i)).expect("send");
        }

        shutdown.store(true, Ordering::Relaxed);
        drop(tx);

        task.await.expect("join").expect("consumer ok");
        assert_eq!(encode_count.load(Ordering::Relaxed), 8);
        assert_eq!(feed_count.load(Ordering::Relaxed), 8);

        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn monitor_receives_clean_pcm_before_shutdown() {
        let encode_count = Arc::new(AtomicUsize::new(0));
        let feed_count = Arc::new(AtomicUsize::new(0));
        let shutdown = Arc::new(AtomicBool::new(false));
        let (progress_tx, _) = broadcast::channel(8);
        let (path, writer) = temp_writer("monitor").await;
        let monitor = Arc::new(RecordingMonitor::default());

        let codec: Arc<Mutex<Box<dyn AudioEncoder>>> =
            Arc::new(Mutex::new(Box::new(CountingEncoder {
                encode_count: Arc::clone(&encode_count),
                delay: Duration::ZERO,
            })));
        let speech: Option<Arc<dyn SpeechFeeder>> = Some(Arc::new(CountingSpeech {
            feed_count: Arc::clone(&feed_count),
            samples: Arc::new(Mutex::new(Vec::new())),
        }));

        let (tx, rx) = broadcast::channel(64);
        let (ctx, _) = context(
            codec,
            speech,
            writer,
            Arc::clone(&shutdown),
            progress_tx,
            Some(Arc::clone(&monitor) as Arc<dyn AudioMonitor>),
        );
        let task = spawn_consumer(rx, ctx);

        let chunk = vec![0.1, -0.1, 0.2, -0.2];
        tx.send(AudioChunk::new(chunk.clone(), 20)).expect("send");
        tokio::time::sleep(Duration::from_millis(30)).await;
        shutdown.store(true, Ordering::Relaxed);
        drop(tx);
        task.await.expect("join").expect("consumer ok");

        assert_eq!(monitor.write_count.load(Ordering::Relaxed), 1);
        assert_eq!(
            monitor.samples.lock().expect("lock").as_slice(),
            [chunk.as_slice()]
        );
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn monitor_write_failure_does_not_abort_consumer() {
        let encode_count = Arc::new(AtomicUsize::new(0));
        let feed_count = Arc::new(AtomicUsize::new(0));
        let shutdown = Arc::new(AtomicBool::new(false));
        let (progress_tx, _) = broadcast::channel(8);
        let (path, writer) = temp_writer("monitor-fail").await;
        let monitor = Arc::new(RecordingMonitor::default());
        monitor.fail_writes.store(true, Ordering::Relaxed);

        let codec: Arc<Mutex<Box<dyn AudioEncoder>>> =
            Arc::new(Mutex::new(Box::new(CountingEncoder {
                encode_count: Arc::clone(&encode_count),
                delay: Duration::ZERO,
            })));
        let speech: Option<Arc<dyn SpeechFeeder>> = Some(Arc::new(CountingSpeech {
            feed_count: Arc::clone(&feed_count),
            samples: Arc::new(Mutex::new(Vec::new())),
        }));

        let (tx, rx) = broadcast::channel(64);
        let (ctx, _) = context(
            codec,
            speech,
            writer,
            Arc::clone(&shutdown),
            progress_tx,
            Some(Arc::clone(&monitor) as Arc<dyn AudioMonitor>),
        );
        let task = spawn_consumer(rx, ctx);

        tx.send(AudioChunk::new(vec![0.1, -0.1], 20)).expect("send");
        tokio::time::sleep(Duration::from_millis(30)).await;
        shutdown.store(true, Ordering::Relaxed);
        drop(tx);
        task.await.expect("join").expect("consumer ok");

        assert_eq!(encode_count.load(Ordering::Relaxed), 1);
        assert_eq!(feed_count.load(Ordering::Relaxed), 1);
        let _ = std::fs::remove_file(path);
    }
}

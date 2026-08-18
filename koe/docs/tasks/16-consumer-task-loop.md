---
title: 16 — Consumer Task Loop
status: draft
depends: [15-pipeline-core]
spec_refs: [10-recording-pipeline]
---

# 16 — Consumer Task Loop

Implement the async task loop that consumes audio from the broadcast channel.

## Location

`koe-core/src/pipeline/consumer.rs`

## Architecture

```
Ring Buffer → FFI callback → AEC → tokio::broadcast → [Encoder, ASR Feeder]
```

The broadcast channel fans out clean audio to two consumers running on the
same tokio task.

## Implementation

```rust
async fn consumer_loop(
    mut rx: tokio::sync::broadcast::Receiver<AudioChunk>,
    encoder: Arc<Mutex<Box<dyn AudioEncoder>>>,
    speech: Arc<SpeechAnalyzerHandle>,
    writer: Arc<FileWriter>,
    shutdown: Arc<AtomicBool>,
) {
    loop {
        match rx.recv().await {
            Ok(chunk) => {
                // 1. Encode: blocking work → spawn_blocking
                let encoder = encoder.clone();
                let encoded = tokio::task::spawn_blocking(move || {
                    encoder.lock().unwrap().encode(&chunk.samples)
                }).await.unwrap();

                // 2. Write to disk
                writer.write(&encoded?).await?;

                // 3. Feed to speech analyzer (non-blocking on Rust side)
                speech.feed_audio(chunk.samples);

                // 4. Emit progress (via separate broadcast channel for CLI/GUI)
            }
            Err(RecvError::Lagged(n)) => {
                log::warn!("Consumer lagged by {} chunks; audio dropped", n);
                // Heal: skip ahead, no recovery possible for lost audio
            }
            Err(RecvError::Closed) => break,
        }

        if shutdown.load(Ordering::Relaxed) {
            break;
        }
    }
    // Drain remaining chunks...
}
```

## AudioChunk

```rust
pub struct AudioChunk {
    pub samples: Vec<f32>,      // Can Be interleaved stereo
    pub timestamp_ms: u64,       // Monotonic clock
    pub frame_count: usize,
}
```

## Performance Budget

- Encoding: `spawn_blocking` — OGG encode of 960 frames should be < 1 ms
- Disk write: async I/O via tokio
- ASR feed: non-blocking (native side buffers internally)

## Lag Handling

If consumer lags (broadcast channel buffer full, oldest chunks dropped):
1. Log warning with lag count
2. Continue with next available chunk
3. Drop counter is incremented; user sees gap in recording

## Verification

- Feed audio chunks at real-time rate, verify all reach encoder and ASR
- Simulate lag by slowing consumer, verify `RecvError::Lagged` handled gracefully
- Verify graceful shutdown: no panic, all resources cleaned up

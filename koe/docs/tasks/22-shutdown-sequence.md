---
title: 22 — Shutdown Sequence
status: draft
depends: [15-pipeline-core, 16-consumer-task-loop]
spec_refs: [10-recording-pipeline]
---

# 22 — Graceful Shutdown & Buffer Drain

Implement the orderly shutdown sequence.

## Location

`koe-core/src/pipeline/shutdown.rs` (or part of `pipeline.rs`)

## Shutdown Sequence

```
User Stop / Ctrl-C
  → Set shutdown flag (AtomicBool)
  → Stop native capture (FFI call)
  → Wait for audio callback to stop firing (~one block, < 20ms)
  → Drain ring buffer (process remaining frames)
  → Finalize speech analyzer (SFSpeechAnalyzer.finalize())
    → Flush any partial segments as final
  → Finalize encoder (write OGG trailer / WAV header / FLAC footer)
  → Flush file writes
  → Finalize transcript formatter
  → Write transcript file
  → Emit RecordingSummary
```

## Timing Requirement

Complete within 2 seconds of user request.
In practice: ring buffer capacity is 4×20ms = 80ms max drain time.
Encoder finalization: ~10ms. Total: < 100ms.

## Signal Handling for CLI

| Signal | Behavior |
|--------|----------|
| SIGINT (1st) | Initiate graceful shutdown. Exit code 5. |
| SIGINT (2nd, within 2s) | Force stop without flushing (may lose partial segments). |
| SIGTERM | Same as SIGINT 1st press. |
| SIGUSR1 | Toggle pause/resume. |

## Implementation Outline

```rust
impl RecordingPipeline {
    pub async fn stop(&mut self) -> Result<RecordingSummary, PipelineError> {
        // 1. Signal native capture to stop
        self.native_capture.stop().await?;

        // 2. Wait for ring buffer drain
        // (consumer loop will process remaining chunks and exit)

        // 3. Finalize speech analyzer
        let final_segments = self.speech.finalize().await?;
        for seg in &final_segments {
            self.transcript_fmt.write_segment(seg);
        }
        self.segments.extend(final_segments);

        // 4. Finalize encoder
        let trailer = self.encoder.finalize()?;
        self.file_writer.write_all(&trailer).await?;

        // 5. Finalize transcript
        let transcript = self.transcript_fmt.finalize();
        self.transcript_writer.write_all(transcript.as_bytes()).await?;

        // 6. Flush and close
        self.file_writer.flush().await?;
        self.transcript_writer.flush().await?;

        // 7. Compute summary
        Ok(RecordingSummary {
            duration_sec: self.elapsed().as_secs_f64(),
            bytes_written: self.bytes_written,
            transcript_segment_count: self.segments.len() as u64,
            dropped_audio_frames: self.drop_counter.load(Ordering::Relaxed),
            format: self.config.audio_format.clone(),
        })
    }
}
```

## Verification

- Start recording, stop immediately → verify clean shutdown
- Start recording, feed audio, stop → verify all audio processed before exit
- Test double SIGINT → verify force exit does not corrupt files
- Test pause → stop → verify final segments include pre-pause content

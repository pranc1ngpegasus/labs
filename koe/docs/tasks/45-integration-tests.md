---
title: 45 — Integration Tests
status: draft
depends: [15-pipeline-core, 21-echo-cancellation, 17-audio-encoder-trait-and-ogg, 20-transcript-formatter]
spec_refs: [10-recording-pipeline, 05-echo-cancellation]
---

# 45 — Integration Tests

End-to-end pipeline tests covering critical paths.

## Location

`koe-core/tests/`

## Test Categories

### 1. Audio Encoder Round-Trip Tests

```rust
// tests/encoder_tests.rs
#[test]
fn ogg_encode_decode_roundtrip() {
    let pcm = generate_test_audio(Duration::from_secs(5));
    let mut encoder = OggEncoder::new(OggConfig { quality: 0.4, sample_rate: 48000, channels: 2 });
    let encoded = encoder.encode(&pcm).unwrap();
    let trailer = encoder.finalize().unwrap();
    let full_ogg = [encoded, trailer].concat();

    // Decode with ffmpeg CLI or a Rust decoder
    let decoded = decode_ogg(&full_ogg);
    // Compare RMS (tolerance for lossy compression)
    assert_rms_similar(&pcm, &decoded, 0.05);
}
```

### 2. AEC Synthetic Tests

```rust
// tests/aec_tests.rs
#[test]
fn aec_silence_cancellation() {
    let far_end = generate_sine(440.0, Duration::from_secs(1));
    let near_end = far_end.clone(); // Near-end = echo of far-end
    let mut aec = AcousticEchoCanceller::new(AecConfig::default());

    // Let filter adapt
    for _ in 0..200 {
        aec.process_block(&far_end[..256], &near_end[..256]);
    }

    // Now near-end = silence → output should be near silence
    let near_silence = vec![0.0f32; 256];
    let output = aec.process_block(&far_end[..256], &near_silence);
    let output_rms = rms(&output);

    // ERLE should be > 20 dB
    let erle = 20.0 * (rms(&far_end[..256]) / rms(&output)).log10();
    assert!(erle > 20.0, "ERLE = {} dB", erle);
}
```

### 3. Pipeline Lifecycle Tests

```rust
// tests/pipeline_tests.rs
#[tokio::test]
async fn pipeline_start_stop() {
    let config = PipelineConfig {
        source: AudioSourceConfig::Microphone,
        output_path: temp_dir().join("test.ogg"),
        locale: "en-US".into(),
        audio_format: OutputFormat::Ogg { quality: 0.4 },
        transcript_format: TranscriptFormat::Txt,
        enable_aec: false,
        comfort_noise: false,
        monitor: false,
    };

    let pipeline = RecordingPipeline::start(config).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await; // Capture for 500ms
    let summary = pipeline.stop().await.unwrap();

    assert!(summary.duration_sec > 0.4);
    assert!(summary.bytes_written > 0);
    assert!(std::fs::metadata(&config.output_path).unwrap().len() > 0);
}

#[tokio::test]
async fn pipeline_pause_resume() {
    // ... verify elapsed time pauses and resumes correctly
}

#[tokio::test]
async fn pipeline_double_interrupt() {
    // ... verify double SIGINT doesn't panic or corrupt files
}
```

### 4. Transcript Formatter Tests

```rust
// tests/transcript_tests.rs
#[test]
fn srt_formatting() {
    let segments = vec![
        TranscriptionSegment { text: "Hello".into(), start_ms: 1000, end_ms: 2000, is_final: true, confidence: 0.95 },
        TranscriptionSegment { text: "World".into(), start_ms: 3000, end_ms: 4000, is_final: true, confidence: 0.92 },
    ];
    let mut fmt = SrtFormatter::new();
    for seg in &segments { fmt.write_segment(seg); }
    let output = fmt.finalize();

    assert!(output.contains("00:00:01,000 --> 00:00:02,000"));
    assert!(output.contains("Hello"));
    assert!(output.contains("2\n00:00:03,000 --> 00:00:04,000"));
}
```

### 5. Ring Buffer Tests (Native + FFI)

```rust
// koe-ffi/tests/ringbuffer_tests.rs
#[test]
fn ring_buffer_overflow() {
    let buf = RingBuffer::new(1024, 2);
    let data = vec![0.5f32; 2048]; // 2x capacity
    assert!(!buf.write(data.as_ptr(), 2048)); // Should fail
    assert_eq!(buf.drop_count(), 1);
}
```

## CI Integration

- Run `cargo test --workspace` in CI
- AEC tests need no audio hardware
- Pipeline tests may need mock FFI layer (use dependency injection or trait-based mocking)

## Verification

```bash
cargo test --workspace
cargo test --workspace -- --ignored  # Integration tests that need macOS
```

---
title: 17 — AudioEncoder Trait & OGG Vorbis Encoder
status: draft
depends: [15-pipeline-core]
spec_refs: [11-data-formats]
---

# 17 — AudioEncoder Trait & OGG Vorbis

Implement the encoder abstraction and OGG Vorbis encoder.

## Location

`koe-core/src/codec/`

```
koe-core/src/codec/
  mod.rs      — Codec trait + OutputFormat enum + registry
  ogg.rs      — OGG Vorbis encoder
```

## AudioEncoder Trait

```rust
pub trait AudioEncoder: Send {
    /// Encode a chunk of PCM audio. Returns encoded bytes (may be empty if
    /// the encoder buffers internally).
    fn encode(&mut self, pcm: &[f32]) -> Result<Vec<u8>, CodecError>;

    /// Flush any buffered frames and finalize the stream (write trailer).
    fn finalize(&mut self) -> Result<Vec<u8>, CodecError>;
}

pub enum OutputFormat {
    Ogg { quality: f32 },
}

#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    #[error("Encoder error: {0}")]
    Encoder(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}
```

## OGG Vorbis Encoder

### Library
Use `vorbis-encoder` crate (pure Rust) or bindings to `libvorbis`.

### Configuration
| Parameter | Value |
|-----------|-------|
| Quality | 0.4 (speech-optimized) |
| Sample rate | 48,000 Hz |
| Channels | 2 |
| Block size | 256–2048 samples (adaptive) |

### OGG Container
- Page-based streaming container
- Write Identification Header → Vorbis Comment → Setup Header → Audio Pages

### Vorbis Comment Tags
| Tag | Content |
|-----|---------|
| `TITLE` | `{app_name} recording — {date} {time}` |
| `ARTIST` | `Koe` |
| `DATE` | ISO 8601 recording start |
| `DESCRIPTION` | `Source: {source}, Locale: {locale}` |
| `ENCODER` | `koe v{version}` |
| `KOE_SOURCE` | JSON of `AudioSourceConfig` |

### Quality Level Justification
0.4 quality ≈ ~128 kbps nominal, speech-optimized. Balances quality,
file size, and encode speed.

## Verification

- Encode 10 seconds of test audio, decode with ffmpeg, verify quality
- Verify OGG headers are valid (use `ogginfo` or `ffprobe`)
- Verify Vorbis Comment tags are written correctly
- Measure encode latency per block (must be < 1 ms per 960-frame block)

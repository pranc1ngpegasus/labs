---
title: 19 — FLAC Encoder
status: draft
depends: [17-audio-encoder-trait-and-ogg]
spec_refs: [11-data-formats]
---

# 19 — FLAC Encoder

Implement FLAC lossless encoder as an archival option.

## Location

`koe-core/src/codec/flac.rs`

## Format

```
fLaC magic
  STREAMINFO block (sample rate, channels, bit depth, total samples)
  PADDING block
  VORBIS_COMMENT block (metadata)
  FRAME₀ | FRAME₁ | ... | FRAMEₙ (audio frames)
```

## Configuration

| Property | Value |
|----------|-------|
| Compression level | 5 (default balance) |
| Block size | 4096 samples (~85 ms at 48 kHz) |
| Bits per sample | 24 (converted from f32) |
| Channels | 2 |

## Implementation

- Implement `AudioEncoder` trait
- Use `flacenc-rs` or `claxon` for encoding, or bind to `libFLAC`
- `encode()`: feed PCM block → compress → accumulate FLAC frames
- `finalize()`: write remaining frames, update STREAMINFO with total sample count
- Write Vorbis Comments (same metadata as OGG)

## Compression Ratio

Target: ~50–60% of raw PCM size (speech content).

## Verification

- Encode test audio, verify with `ffprobe` or `flac --test`
- Verify lossless round-trip: encode → decode → compare with original
- Verify Vorbis Comment metadata
- Measure encode speed (should handle real-time at 48 kHz)

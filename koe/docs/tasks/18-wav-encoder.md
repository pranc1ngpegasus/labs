---
title: 18 — WAV Encoder
status: draft
depends: [17-audio-encoder-trait-and-ogg]
spec_refs: [11-data-formats]
---

# 18 — WAV Encoder

Implement WAV (RIFF/WAVE) encoder as a lossless fallback.

## Location

`koe-core/src/codec/wav.rs`

## Format

```
RIFF header ("RIFF" + file size)
  fmt  chunk (PCM, 48k/2ch/f32 or configurable bit depth)
  fact chunk (sample count)
  data chunk (raw interleaved PCM)
```

## Configuration

| Property | Default | Options |
|----------|---------|---------|
| Sample rate | 48,000 Hz | Fixed |
| Channels | 2 | 1–2 |
| Bit depth | 32-bit float | 16-bit int, 24-bit int (via conversion) |
| Byte order | Little-endian | Native on Apple Silicon |

## Implementation

- Implement `AudioEncoder` trait
- `encode()` appends PCM to an internal buffer; returns empty until `finalize()`
- `finalize()` writes the full RIFF header + data chunk to a `Vec<u8>`
- Note: WAV has a 4 GB limit (RIFF64 not implemented in v1)

## Caveats

- 1 hour at 48 kHz stereo f32 = ~2.1 GB → near the 4 GB limit
- Warn users recording > 2 hours to use OGG or FLAC instead
- For streaming writes, consider writing header with placeholder size,
  then seeking back to patch — but this requires `Seek` on the output file

## Verification

- Generate WAV file from test data, verify with `ffprobe`
- Verify `fmt`, `fact`, `data` chunks are valid
- Test with both f32 and i16 bit depths
- Test with mono input

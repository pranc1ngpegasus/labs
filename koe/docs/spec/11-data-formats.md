---
title: Data Formats
topic: data-formats
status: draft
date: 2026-08-10
depends: [01-architecture]
---

# 11 — Data Formats

## Audio Output Formats

One output format, selected via `--format <FORMAT>`:

| Format | Extension | Compression | Container | Metadata Support |
|--------|-----------|-------------|-----------|-----------------|
| **OGG** (default) | `.ogg` | Lossy (~8–12% of raw PCM) | OGG container | Vorbis Comment |

### Canonical PCM Specification

The encoder receives the same input:

| Property | Value |
|----------|-------|
| Sample rate | 48,000 Hz |
| Bit depth | 32-bit float (f32) |
| Channels | 2 (stereo, interleaved L/R) |
| Byte order | Native endian (little-endian on Apple Silicon) |

The OGG encoder consumes `f32` samples directly (no conversion).

### OGG Vorbis Details (Default)

OGG Vorbis is an open, patent-free lossy codec in an OGG container. It
provides excellent speech quality at a fraction of raw PCM size — ideal for
long recording sessions.

```mermaid
flowchart LR
    subgraph OGG["OGG Container"]
        direction LR
        ID["Identification<br/>Header"]
        COMMENT["Vorbis Comment<br/>(metadata)"]
        SETUP["Setup Header<br/>(codebooks)"]
        PAGES["Audio Pages<br/>page₀ │ page₁ │ ... │ pageₙ"]
    end

    ID --> COMMENT --> SETUP --> PAGES
```

- **Quality level:** 0.4 (speech-optimized; roughly equivalent to ~128 kbps
  nominal). Balances quality, file size, and encode speed.
- **Block size:** 256–2048 samples (Vorbis adaptive; small for transients,
  large for tonal)
- **Sample format:** Float32 (Vorbis encoder accepts f32 natively — no
  quantization loss in encoding step)
- **Vorbis Comment tags written:**

| Tag | Source |
|-----|--------|
| `TITLE` | `{app_name} recording — {date} {time}` |
| `ARTIST` | `Koe` |
| `DATE` | ISO 8601 recording start |
| `DESCRIPTION` | `Source: {source_config}, Locale: {locale}` |
| `ENCODER` | `koe v{version}` |
| `KOE_SOURCE` | JSON of `AudioSourceConfig` |

### Encoder Crate

```mermaid
graph TD
    CODEC["koe-core/src/codec/"]
    MOD["mod.rs — Codec trait + registry"]
    OGG["ogg.rs — OGG Vorbis encoder (via libvorbis)"]
    PL["pipeline.rs — Re-exports"]

    CODEC --> MOD
    CODEC --> OGG
    CODEC --> PL
```

```rust
pub trait AudioEncoder: Send {
    fn encode(&mut self, pcm: &[f32]) -> Result<Vec<u8>, CodecError>;
    fn finalize(&mut self) -> Result<Vec<u8>, CodecError>;
}

pub enum OutputFormat {
    Ogg { quality: f32 },
}
```

## Transcript Output Formats

Four format options, selected via `--transcript-format <FORMAT>`:

### TXT (Plain Text)

```
This is the first utterance.
This is the second utterance.
```

- No timestamps, no speaker labels.
- One line per finalized segment.
- Partial segments are not written (only in-memory/on-screen).

### SRT (SubRip)

```srt
1
00:00:01,250 --> 00:00:04,800
This is what was spoken in the first utterance.

2
00:00:05,100 --> 00:00:09,200
This is the second utterance, which is longer.
```

- Timestamps use recording-relative time (not wall clock).
- Segment index is sequential.
- Milliseconds precision.

### VTT (WebVTT)

```vtt
WEBVTT

1
00:00:01.250 --> 00:00:04.800
This is what was spoken in the first utterance.

2
00:00:05.100 --> 00:00:09.200
This is the <i>second</i> utterance, which is longer.
```

- Same timestamp model as SRT.
- Optional styling cues (italic for partial segments in final output? No — partial
  segments are excluded from VTT; only finalized segments).
- WEBVTT header line.

### JSON (Structured)

```json
{
  "format": "koe-transcript",
  "version": 1,
  "locale": "en-US",
  "created_at": "2026-08-10T15:30:00+09:00",
  "source": {
    "type": "system",
    "app_bundle_id": "com.google.Chrome"
  },
  "segments": [
    {
      "index": 0,
      "start_ms": 1250,
      "end_ms": 4800,
      "text": "This is what was spoken in the first utterance.",
      "confidence": 0.95
    },
    {
      "index": 1,
      "start_ms": 5100,
      "end_ms": 9200,
      "text": "This is the second utterance, which is longer.",
      "confidence": 0.92
    }
  ]
}
```

## Transcript Formatter Trait

```rust
pub trait TranscriptFormatter: Send {
    /// Write a finalized segment.
    fn write_segment(&mut self, segment: &TranscriptionSegment);

    /// Get the in-progress output (for CLI preview / GUI live view).
    fn current_output(&self) -> String;

    /// Finalize and return the complete output.
    fn finalize(self) -> String;
}
```

## File Naming Convention

Default output file names when `--output` is not specified:

```
{output_directory}/{app_name}_{date}_{time}.{ext}

Examples:
~/Recordings/Koe/Google Chrome_2026-08-10_153000.ogg
~/Recordings/Koe/Google Chrome_2026-08-10_153000.srt
```

When `--output` is a directory, the default name is used within that directory.
When `--output` is a full path, it is used as-is.

## File Size Estimates

| Duration | OGG Vorbis (q=0.4) | Transcript (SRT) |
|----------|---------------------|-------------------|
| 10 min | ~7 MB | ~20 KB |
| 30 min | ~21 MB | ~60 KB |
| 1 hour | ~42 MB | ~120 KB |
| 2 hours | ~84 MB | ~240 KB |

**Disk space check:** Before recording starts, Koe checks available disk
space on the output volume. If free space < estimated size × 2, it warns the
user (CLI: stderr warning; GUI: banner). If free space < estimated size, it
refuses to start.

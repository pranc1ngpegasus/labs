---
title: CLI Interface
topic: cli
status: draft
date: 2026-08-10
depends: [01-architecture, 06-permission-model, 11-data-formats]
---

# 08 — CLI Interface (`koe-cli`)

## Binary

```bash
koe [GLOBAL_FLAGS] <COMMAND> [ARGS]
```

`koe` is a single binary. Subcommands model the user's workflow:

| Command | Purpose |
|---------|---------|
| `koe record` | Start a recording with transcription |
| `koe list` | List capture-able apps and devices |
| `koe transcribe` | Transcribe an existing audio file (offline) |
| `koe permissions` | Check and diagnose permissions |
| `koe info` | Show device/format/supported-locale info |
| `koe completions` | Shell completion script generation |

## Global Flags

```
--verbose, -v          Increase log verbosity (repeatable: -v, -vv, -vvv)
--quiet, -q            Suppress non-error output
--config <PATH>        Path to config file (default: ~/.config/koe/config.toml)
--help, -h             Print help
--version, -V          Print version
```

## `koe record`

The primary command. Starts capturing, transcribing, and writing output.

```
koe record [OPTIONS] --output <PATH>

Source Selection (mutually exclusive groups):
  --source <SOURCE>              system, mic, or both (required unless
                                 --app-id/--pid are given, which imply system)
  --app-id <BUNDLE_ID>           Capture audio from a specific app
  --pid <PID>                    Capture audio from a process by PID
  --display <DISPLAY_ID>         Capture from a specific display
  --list-sources                 Print available sources and exit

Audio Options:
  --sample-rate <HZ>             Output sample rate (default: 48000)
  --channels <N>                 Output channels: 1 (mono) or 2 (stereo) (default: 2)
  --no-aec                       Disable acoustic echo cancellation
  --no-comfort-noise             Disable comfort noise in AEC output

Transcription Options:
  --locale <LOCALE>              Speech recognition locale (default: en-US)
  --no-transcribe                Record only; skip transcription
  --list-locales                 Print supported locales and exit

Output Options:
  --output, -o <PATH>            Output file path (required)
  --format <FORMAT>              Audio format: ogg
  --transcript-format <FMT>      Transcript format: txt, srt, vtt, json (default: txt)
  --transcript-output <PATH>     Transcript file path (default: <output>.<fmt>)

Recording Options:
  --duration <DURATION>          Max recording duration (e.g., 30m, 1h, 2h30m)
  --max-size <SIZE>              Max output file size (e.g., 500M, 2G)
  --silence-timeout <DURATION>   Stop after N seconds of silence (default: none)
  --monitor, -m                  Play captured audio through output device (monitoring)
```

### Exit Codes

| Code | Meaning |
|------|---------|
| 0 | Recording completed successfully |
| 1 | Permission denied (tip: run `koe permissions`) |
| 2 | Invalid arguments |
| 3 | Capture error (app quit, stream interrupted) |
| 4 | Disk full or I/O error |
| 5 | Interrupted by SIGINT (partial output written) |
| 6 | Internal error (unexpected failure) |

### STDERR Progress Output

While recording, Koe renders a live-updating status block on stderr. When
stderr is not a TTY, it falls back to periodic newline-delimited JSON:

```
# TTY mode:
Recording | ⣾ 00:02:34 | OGG 48kHz stereo | App: Google Chrome (PID 4201)
[SYS] [00:02:30] "This is what I heard..."
# ... newer partials overwrite the line in place ...
[SYS] [00:02:30] "This is what I heard all together."

# Non-TTY mode (--output-format json on stderr):
{"type":"status","elapsed_ms":154000,"size_bytes":1843200}
{"type":"segment","start_ms":150000,"end_ms":152400,"text":"This is what I heard","is_final":false}
```

### Example Sessions

```bash
# Record system audio from Chrome, transcribe en-US
koe record --source system --app-id com.google.Chrome --output meeting.ogg

# Record microphone only, no transcription, 30 minutes max
koe record --source mic --no-transcribe --duration 30m --output voice-memo.ogg

# Record a Zoom call (system + mic) with AEC, Japanese transcription
koe record --source both --app-id us.zoom.xos --locale ja-JP \
  --output zoom-call.ogg --transcript-format srt

# Interactive source selection
koe record --list-sources
# → prints numbered list, user re-runs with --app-id or --pid
```

## `koe list`

```
koe list [OPTIONS]

Options:
  --audio-only         Only show apps with active audio
  --json               Output as JSON array

Output (text mode):
  PID    NAME                  BUNDLE ID               HAS AUDIO
  ─────  ────────────────────  ──────────────────────  ─────────
  4201   Google Chrome         com.google.Chrome        yes
  8891   Spotify               com.spotify.client       yes
  1234   Finder                com.apple.Finder         no
```

## `koe transcribe`

Transcribe an existing audio file without recording.

```
koe transcribe [OPTIONS] <INPUT_FILE>

Options:
  --locale <LOCALE>           Speech recognition locale (default: en-US)
  --output, -o <PATH>         Output transcript path (default: <input>.<format>)
  --format <FORMAT>           Transcript format: txt, srt, vtt, json (default: txt)
  --start-at <TIMESTAMP>      Start transcribing from offset (e.g., 1m30s)
  --end-at <TIMESTAMP>        Stop transcribing at offset
```

Supported input formats: WAV (PCM, Float32), FLAC, OGG, MP3, AAC, AIFF.

## `koe permissions`

```
koe permissions [OPTIONS]

Options:
  --json               Output as JSON

Output (text mode):
  PERMISSION          STATUS      FIX
  ─────────────────   ─────────   ──────────────────────────────────────
  Microphone          Authorized
  Screen Recording    Denied      Open System Settings → Screen Recording
  Accessibility       Denied      Open System Settings → Accessibility

If the terminal has permissions issues:
  Note: Permissions for "Terminal.app" differ from "Koe.app" (GUI).
  The GUI handles permission prompts automatically.
```

## `koe info`

```
koe info [OPTIONS]

Options:
  --json               Output as JSON

Prints:
  - macOS version
  - Default audio input device name and UID
  - Default audio output device name and UID
  - Supported locales for on-device speech recognition
  - Available disk space on output volume
  - Koe version, build target, feature flags
```

## Configuration File

```toml
# ~/.config/koe/config.toml

[defaults]
source = "system"
format = "ogg"
locale = "en-US"
transcript-format = "txt"
sample-rate = 48000
channels = 2

[aec]
enabled = true
comfort-noise = true

[output]
directory = "~/Recordings/Koe"

[transcription]
locale = "en-US"
transcript-format = "srt"
```

CLI flags override config file values. Config file is optional — all values
have defaults.

## Signal Handling

| Signal | Behavior |
|--------|----------|
| SIGINT (Ctrl-C) | Graceful stop: finalize transcription, flush output, write summary. Exit code 5. |
| SIGTERM | Same as SIGINT. |
| SIGUSR1 | Toggle pause/resume. |

During recording, SIGINT is caught on first press. A second SIGINT within 2
seconds forces immediate exit (may lose partial transcript segments).

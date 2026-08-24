# oto

> 音 (sound) — オフライン録音ツール。マイク入力を WAV または Ogg/Opus でファイルに残す。

`oto` is a cross-platform offline microphone recorder. It captures the default
(or a selected) input device and writes either **WAV** (lossless, preserving
the source format) or **Ogg/Opus** (compressed, RFC 7845) to a local file.

- Recording is fully local — no network access, no telemetry.
- Capture and device enumeration go through
  [shiguredo/audio-device-rs](https://github.com/shiguredo/audio-device-rs)
  (CoreAudio / PulseAudio / WASAPI under one API).
- Opus encoding uses [shiguredo/opus-rs](https://github.com/shiguredo/opus-rs)
  with the Ogg container assembled in pure Rust.
- A long-term roadmap adds offline transcription (the design docs describe how
  it relates to the macOS-only `koe`).

## Install / build

```console
nix build .#oto          # packaged via the repo flake
```

macOS / Linux are built and packaged by the flake; Windows builds via a plain
`cargo build` (WASAPI backend).

## Usage

```console
oto list                       # enumerate input devices (--json for machine output)
oto record memo.ogg            # Ogg/Opus (the default); Ctrl-C or --duration to stop
oto record backup.wav          # WAV, source format preserved
oto record --device "USB Mic" --bitrate 96 --duration 90 out.ogg
```

| Flag | Default | Meaning |
| --- | --- | --- |
| `OUTPUT` | `oto-<timestamp>.ogg` | Output path; the extension selects the format |
| `--device` | default input | `unique_id` exact match, then case-insensitive name match |
| `--channels` | `1` | Requested channels; the device's actual count is used |
| `--bitrate` | `64` | Opus bitrate in kbps (ignored for WAV) |
| `--duration` | until Ctrl-C | Stop automatically after N seconds (e.g. `90` or `1.5`) |
| `--format` | extension (default Ogg/Opus) | Force `wav` or `ogg` |
| `--quiet` | off | Suppress the progress line |

The format is decided by the extension: `.wav` → WAV, `.ogg`/`.opus` →
Ogg/Opus, anything else → Ogg/Opus (the default). `--format` overrides it.

Recording stops gracefully on Ctrl-C / SIGTERM / `--duration`, finalizing the
output (WAV header rewrite, final Ogg page). A second Ctrl-C within the stop
window forces exit (code 5).

## Exit codes

| Code | Meaning |
| --- | --- |
| 0 | Success (recording finished, including a single Ctrl-C) |
| 2 | Invalid arguments |
| 3 | Capture / device error (mic permission hint included) |
| 4 | File I/O error |
| 5 | Interrupt (second Ctrl-C force exit) |
| 6 | Internal error |

## Design

- **Offline-first**: the recording pipeline never touches the network.
- **Format-preserving WAV**: `S16` frames stay PCM16, `F32` frames stay IEEE
  float32, at the device's actual rate/channels.
- **Ogg/Opus (RFC 7845)**: i16 conversion, downmix to the requested channels,
  and resampling to a supported Opus rate (e.g. 44.1 → 48 kHz) only when
  needed.
- **Platform-agnostic encode layer**: conversion, encoders, and containers are
  a pure-Rust layer tested headlessly (no device required in CI). Real-device
  recording is verified manually; see the manual checklist in
  [06-testing](./docs/spec/06-testing.md).

The full design spec (`docs/spec/`, Japanese) covers requirements, CLI
interface, encoding details, Nix packaging, testing, and the implementation
plan.

**Crate layout:** `oto-capture` (device enumeration + capture session) /
`oto-encode` (conversion + WAV / Ogg+Opus encoders) / `oto-core` (recording
pipeline + session) / `oto-cli` (binary). Dependencies flow one way:
`oto-cli → oto-core → {oto-capture, oto-encode}`.

## License notes

`oto` links `shiguredo_opus` (Apache-2.0), which statically links **libopus**
(Xiph, BSD-3-Clause). See [THIRD-PARTY.md](./THIRD-PARTY.md) for the libopus
attribution and license text.
---
title: Glossary
topic: glossary
status: draft
date: 2026-08-10
depends: []
---

# 12 — Glossary

## Terms

### A

**AEC** — Acoustic Echo Cancellation. Digital signal processing technique
that removes far-end (speaker output) audio from the near-end (microphone)
signal. See [05-echo-cancellation](./05-echo-cancellation.md).

**AudioConverter** — Core Audio API for converting between audio formats
(sample rate, bit depth, codec). Used by Koe for resampling.

**AudioQueue** — Higher-level Core Audio API for playback and recording.
Used by Koe for audio monitoring (pass-through to output device).

**AudioServer Plug-in** — macOS HAL (Hardware Abstraction Layer) extension
mechanism. Core Audio Process Tap is implemented as an AudioServer Plug-in.

### C

**Callback (Real-Time)** — A function invoked by Core Audio on a real-time
priority thread. Must not allocate, lock, or perform I/O. Koe's real-time
callback copies audio into a lock-free ring buffer and returns.

**Comfort Noise** — Low-amplitude noise mixed into AEC output during
echo-only periods to prevent the "dead air" sensation. Configurable on/off.

**Core Audio Process Tap** — A macOS API (`kAudioProcessTapType`) that
inserts a tap into a specific process's audio output graph, capturing audio
before it reaches the hardware output device. See
[02-core-audio-process-tap](./02-core-audio-process-tap.md).

### D

**Double-Talk** — State where both far-end (remote speaker) and near-end
(local speaker) audio are active simultaneously. During double-talk, AEC
filter adaptation is paused to prevent divergence.

**Double-Talk Detection** — Algorithm to detect the double-talk condition.
Koe uses the Geigel algorithm (energy comparison with a threshold).

### E

**ERLE** — Echo Return Loss Enhancement. Ratio of echo energy before
cancellation to echo energy after cancellation, expressed in dB. Higher is
better. Target: > 20 dB.

**Entitlement** — A key-value pair in an app's code signature that declares
required capabilities and permissions. Required for microphone, screen
recording, and audio tap access.

### F

**Far-End** — The audio signal from the remote side of a conversation, played
through the local speakers. In AEC terms, this is the reference signal that
must be removed from the microphone.

**FLAC** — Free Lossless Audio Codec. Lossless compressed audio format.
Available as an archival option in Koe. Compresses PCM audio losslessly
to ~50–60% of original size.

**OGG Vorbis** — Open, patent-free lossy audio codec in an OGG container.
Koe's default output format. Provides excellent speech quality at ~8–12%
of raw PCM size.

**Frontmatter** — YAML metadata block at the top of Markdown files, delimited
by `---`. Used in Koe's spec documents for title, topic, status, and
dependencies.

### G

**GPUI** — GPU-accelerated Rust UI framework developed by Zed Industries.
Koe's GUI (`koe-gui`) is built entirely on GPUI. Provides Metal-backed
rendering, virtual lists, and AppKit window integration.

### H

**HAL** — Hardware Abstraction Layer. The lowest-level Core Audio API for
device enumeration, property queries, and input/output stream management.

### K

**Koe** — The project name. Japanese for "voice" (声). Pronounced "koh-eh."

### M

**Metal** — Apple's low-level GPU API. GPUI renders via Metal on macOS,
providing 60 fps rendering with low CPU overhead.

### N

**Near-End** — The local microphone signal. In AEC terms, this contains the
local speaker's voice plus echo of the far-end signal. AEC removes the
far-end component to produce clean near-end audio.

**NLMS** — Normalized Least Mean Squares. Adaptive filter algorithm used by
Koe's AEC. Normalizes the step size by the input signal power, providing
stable convergence across varying signal levels.

**Notarization** — Apple's app distribution requirement for software
distributed outside the Mac App Store. The app binary is uploaded to Apple
for malware scanning and must pass automated checks.

### P

**Process Tap** — See Core Audio Process Tap.

### R

**Real-Time Thread** — A macOS thread running at a high priority
(`THREAD_TIME_CONSTRAINT_POLICY`) owned by the audio system. Work done on this
thread directly affects audio glitch probability. See Callback (Real-Time).

**Ring Buffer** — A fixed-size circular buffer with lock-free read/write
access. Koe's ring buffer bridges the native audio callback (writer) and the
Rust consumer (reader). Uses atomic operations for indices.

### S

**SCK** — ScreenCaptureKit. Apple's framework for capturing screen content
(per-display, per-window, per-app), including audio. macOS 12.3+. See
[03-screen-capture-kit](./03-screen-capture-kit.md).

**SCShareableContent** — An SCK type representing all capture-able content
(displays, windows, apps) visible to the user. Requires screen recording
permission.

**SFSpeechAnalyzer** — Apple's on-device speech recognition API. macOS 14+.
Accepts streaming audio and produces incremental transcription results. See
[04-speech-recognition](./04-speech-recognition.md).

**SPSC** — Single-Producer, Single-Consumer. A concurrency pattern where one
thread writes and one thread reads. Enables lock-free data structures. Koe's
ring buffer is SPSC.

### T

**TCC** — Transparency, Consent, and Control. macOS privacy subsystem that
manages permission prompts and the Privacy & Security settings pane.

### U

**uniffi-rs** — A Rust library for generating foreign-language bindings
(Swift, Kotlin, Python) from Rust interface definitions. Koe uses uniffi's
proc-macro variant to generate the Swift↔Rust C ABI. See
[07-native-bridge](./07-native-bridge.md).

### V

**VAD** — Voice Activity Detection. Identifies speech vs. silence in an audio
stream. SFSpeechAnalyzer has built-in VAD; Koe does not add a separate one.

**Vorbis Comment** — Metadata block format used by FLAC containers. Koe writes
Vorbis Comments with recording metadata (date, source, app name).

**VTT** — WebVTT (Web Video Text Tracks). A W3C subtitle/caption format. Koe
supports VTT as a transcript output format.

## Abbreviations

| Abbrev. | Expansion |
|---------|-----------|
| ABI | Application Binary Interface |
| AEC | Acoustic Echo Cancellation |
| ASR | Automatic Speech Recognition |
| CAF | Core Audio Format |
| CAPT | Core Audio Process Tap |
| DSP | Digital Signal Processing |
| ERLE | Echo Return Loss Enhancement |
| FFI | Foreign Function Interface |
| FLAC | Free Lossless Audio Codec |
| OGG | OGG container format (not an acronym) |
| GPUI | (Not an acronym; Zed's GPU UI framework) |
| HAL | Hardware Abstraction Layer |
| PCM | Pulse-Code Modulation |
| SCK | ScreenCaptureKit |
| SPSC | Single-Producer, Single-Consumer |
| SRT | SubRip (subtitle format) |
| TCC | Transparency, Consent, and Control |
| UDL | UniFFI Definition Language |
| VAD | Voice Activity Detection |
| VTT | Web Video Text Tracks |

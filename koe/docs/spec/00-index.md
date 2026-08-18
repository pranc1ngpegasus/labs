---
title: Koe — Specification Index
topic: index
status: draft
date: 2026-08-10
---

# Koe — macOS Offline Transcription & Recording Tool

**Koe** (声, Japanese for "voice") is a macOS-native offline transcription and
recording tool. It captures system audio via Core Audio Process Tap and
ScreenCaptureKit, transcribes speech using Apple's on-device SpeechAnalyzer,
and presents results through both CLI and GUI interfaces. All non-native logic
is implemented in Rust; the native layer is a thin Swift shim over macOS
frameworks.

## Design Principles

1. **Offline-first.** No network dependency for core functionality. Speech
   recognition uses Apple's on-device models exclusively.
2. **Privacy-respecting.** All audio stays on-device. No telemetry, no cloud
   uploads.
3. **Permission-aware.** Explicit, minimal permission requests with clear
   user-facing rationale.
4. **Rust-first.** Business logic, pipeline orchestration, CLI/GUI state
   management in Rust. Native bindings are a thin FFI shim.
5. **Composable.** The recording/transcription pipeline is a library crate
   consumed by both CLI and GUI frontends.

## Document Index

| # | Document | Summary |
|---|----------|---------|
| 01 | [Architecture Overview](./01-architecture.md) | Crate topology, process model, data-flow layers |
| 02 | [Core Audio Process Tap](./02-core-audio-process-tap.md) | System-wide audio capture via `kAudioProcessTapType` |
| 03 | [ScreenCaptureKit](./03-screen-capture-kit.md) | Per-app/screen audio capture via `SCStream` |
| 04 | [Speech Recognition](./04-speech-recognition.md) | On-device transcription via `SFSpeechAnalyzer` |
| 05 | [Echo Cancellation](./05-echo-cancellation.md) | AEC design for mixed mic + system-audio scenarios |
| 06 | [Permission Model](./06-permission-model.md) | Entitlements, TCC prompts, user-flow |
| 07 | [Native Bridge (FFI)](./07-native-bridge.md) | Swift ↔ Rust boundary, uniffi design, memory model |
| 08 | [CLI Interface](./08-cli-interface.md) | `koe-cli` subcommands, args, output modes |
| 09 | [GUI Interface](./09-gui-interface.md) | `koe-gui` (GPUI, GPU-accelerated Rust UI) |
| 10 | [Recording Pipeline](./10-recording-pipeline.md) | End-to-end data flow, buffer management, threading |
| 11 | [Data Formats](./11-data-formats.md) | Audio containers, transcript formats, metadata |
| 12 | [Glossary](./12-glossary.md) | Terms and abbreviations |

## Crate Map

```
koe-core/          — Rust library: pipeline, AEC, format codecs, CLI/GUI shared state
koe-native/        — Swift package: thin wrappers over macOS frameworks, exports C ABI
koe-ffi/           — Rust crate: uniffi-generated bindings, type conversions
koe-cli/           — Rust binary: clap-driven CLI frontend
koe-gui/           — Rust binary: GPUI (GPU-accelerated) GUI frontend
```

## Quick-Start (for implementors)

1. Read [01-architecture](./01-architecture.md) for the big picture.
2. Read [07-native-bridge](./07-native-bridge.md) to understand the FFI boundary.
3. Read [06-permission-model](./06-permission-model.md) before touching any
   capture code — macOS entitlements are gating.
4. Read the topic doc relevant to the component you are building.

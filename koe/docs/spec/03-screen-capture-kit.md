---
title: ScreenCaptureKit
topic: audio-capture
status: draft
date: 2026-08-10
depends: [01-architecture, 06-permission-model]
---

# 03 — ScreenCaptureKit

## Overview

ScreenCaptureKit (SCK) is Apple's modern screen and audio capture framework
(macOS 12.3+). It supports per-application, per-window, and per-display capture
of both video and audio. Koe uses SCK **exclusively for its audio capture
capabilities** — video frames are discarded immediately.

SCK is the recommended path for capturing **audio from a specific app** when
the app is not the current user's foreground process. It is also the only
officially supported API for capturing audio from apps that opt into
`com.apple.developer.system-extension.audio-capture`.

## Why Two Capture APIs?

| Capability | Core Audio Process Tap | ScreenCaptureKit |
|------------|----------------------|-------------------|
| Per-app audio capture | Yes (PID-based) | Yes (SCShareableContent) |
| Real-time / low-latency | Yes (audio thread) | Near-real-time (callback) |
| Requires audio tap entitlement | Yes | No |
| Works with sandboxed apps | No | Yes (with user consent) |
| Captures from background apps | No (app must be playing audio) | Yes |
| Video capture | No | Yes (discarded by Koe) |
| macOS minimum | 10.x (varies) | 12.3+ |
| Deprecation risk | High (deprecated API path) | Low (actively maintained) |

The strategy: **prefer SCK for v1** as the primary per-app audio capture path
because it has lower entitlement friction and active Apple support. Use Process
Tap as a fallback for low-latency use cases (e.g., real-time monitoring) and
for macOS < 12.3 compatibility.

## API Surface (Swift)

```swift
// koe-native/Sources/ScreenAudio/ScreenAudioCapture.swift

public final class ScreenAudioCapture: NSObject {
    /// Request permission and enumerate capture-able content.
    /// - Returns: Array of shareable windows/apps/displays with audio
    public static func enumerateContent() async throws -> SCShareableContent

    /// Start capturing audio from a specific target.
    /// - Parameters:
    ///   - target: The SCShareableContent entry to capture
    ///   - config: Stream configuration (width/height = 1×1, queues audio only)
    ///   - callback: Called on SCK's callback queue with audio buffer
    public func start(
        target: SCRunningApplication,    // or SCDisplay/SCWindow
        config: SCStreamConfiguration,
        callback: @escaping (AudioBuffer) -> Void
    ) async throws

    /// Stop the stream.
    public func stop() async
}
```

## Stream Configuration

```swift
let config = SCStreamConfiguration()
config.width = 1             // Minimal video — we discard it
config.height = 1
config.pixelFormat = .bgra8888
config.minimumFrameInterval = CMTime(value: 1, timescale: 1) // 1 fps
config.queueDepth = 3
config.capturesAudio = true  // Audio is what we want
config.excludesCurrentProcessAudio = true  // Don't capture ourselves
config.channelCount = 2
config.sampleRate = 48_000
```

Key details:
- **Video is unavoidable but minimized** — SCK requires at least 1×1 video.
  Frame data is immediately dropped.
- **`excludesCurrentProcessAudio`** prevents feedback loops when Koe itself
  produces audio (e.g., playback of recorded audio, UI sounds).
- **`queueDepth = 3`** keeps latency low without risking drops.

## Callback Behavior

SCK delivers audio via `SCStreamOutput.stream(_:didOutputSampleBuffer:of:)`.
This fires on a **serial dispatch queue** (not the real-time audio thread):

```swift
public func stream(
    _ stream: SCStream,
    didOutputSampleBuffer sampleBuffer: CMSampleBuffer,
    of type: SCStreamOutputType
) {
    guard type == .audio else { return } // Discard video frames

    guard let pcmData = sampleBuffer.audioBufferList?.pointee else { return }

    // Copy PCM into ring buffer (same lock-free SPSC as Process Tap)
    let frameCount = sampleBuffer.numSamples
    ringBuffer.write(pcm: pcmData, frames: frameCount)
}
```

Because this is not a real-time audio thread, we have more flexibility than
with Process Tap. However, we still minimize work: copy to ring buffer, return
quickly.

## Content Picker UI

The GUI frontend uses `SCContentSharingPicker` (macOS 14+) for a system-standard
content picker:

```swift
let picker = SCContentSharingPicker()
picker.isAudioIncluded = true
picker.present()
// Delegate receives didPickContent: SCShareableContent
```

For macOS 12–13, the GUI implements a custom NSWindow with an NSOutlineView
populated from `SCShareableContent`.

The CLI frontend uses `enumerateContent()` and presents a numbered list on
stdout, accepting selection via `--app-id <bundleID>` or `--pid <PID>`.

## Audio Buffer Normalization

Same canonical format as Process Tap (see [02-Core-Audio-Process-Tap](./02-core-audio-process-tap.md#format-normalization)):

| Property | Canonical Value |
|----------|-----------------|
| Sample rate | 48,000 Hz |
| Channels | 2 (stereo), interleaved |
| Sample format | Float32 |

SCK natively supports configuring the output format via `SCStreamConfiguration`,
so no post-hoc resampling is needed in most cases.

## Error Scenarios

| Error | Cause | Recovery |
|-------|-------|----------|
| `SCStreamError.userDeclined` | User denied screen recording permission | Show Settings link, exit gracefully |
| `SCStreamError.noAudioSamples` | Target stopped producing audio | Pause, notify UI; resume when samples resume |
| `SCStreamError.applicationUnavailable` | Target app quit | End capture for this target; notify user |
| `SCStreamError.frameRateLimitExceeded` | Internal (should not happen at 1fps) | Log warning, continue |

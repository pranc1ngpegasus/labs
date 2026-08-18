---
title: 07 — Core Audio Process Tap
status: draft
depends: [04-ring-buffer, 05-audio-format-normalization]
spec_refs: [02-core-audio-process-tap]
---

# 07 — Core Audio Process Tap

Implement system-wide audio capture via `kAudioProcessTapType`.

## Location

`koe-native/Sources/AudioTap/AudioTap.swift`

## API

```swift
public final class AudioTap {
    public init(pid: pid_t) throws
    public func start(callback: @escaping AudioTapCallback) throws
    public func stop()
}

public typealias AudioTapCallback = (
    _ buffer: UnsafePointer<Float>,
    _ frameCount: Int,
    _ streamDesc: AudioStreamBasicDescription
) -> Void
```

## Implementation

### macOS 14+ path (Audio Server Plug-in API)
1. Use `kAudioServerPlugIn` API to insert a tap on the target PID
2. Configure tap format via `AudioObjectSetPropertyData(kAudioProcessTapProperty...)`
3. Register a C-callable callback that fires on the real-time audio thread

### macOS 13 fallback (deprecated `AudioHardwareCreateProcessTap`)
1. Compile-time `#if available(macOS 14, *)` branching
2. Use `AudioHardwareCreateProcessTap()` with PID
3. Same callback signature, but different setup

### Setup sequence
1. `AudioObjectGetPropertyData(kAudioHardwarePropertyDefaultOutputDevice)` — find output device
2. `AudioObjectGetPropertyData(kAudioDevicePropertyDeviceUID)` — get UID for tap
3. Create tap on target PID
4. Apply format configuration

### Real-Time Callback
- **Must follow all real-time safety rules** (no allocation, no locks, no I/O, no ObjC messages)
- Does exactly: copy buffer to RingBuffer, advance write pointer
- If ring buffer write fails, increment drop counter silently

## Limitations
- One tap per process maximum
- No system-wide "tap everything"
- Requires `com.apple.security.cs.disable-library-validation` entitlement
- Not available in App Sandbox

## Verification
- Install tap on a known audio-emitting PID
- Verify audio data arrives in ring buffer
- Verify no audible impact on target app
- Test on both macOS 14+ and 13 (if available)

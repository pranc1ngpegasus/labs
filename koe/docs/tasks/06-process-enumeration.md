---
title: 06 — Process Enumeration
status: draft
depends: [02-koe-native-package]
spec_refs: [02-core-audio-process-tap, 03-screen-capture-kit]
---

# 06 — Audio Process Enumeration

Enumerate running applications with active audio output.

## Location

`koe-native/Sources/AudioUtils/ProcessEnumerator.swift`

## API

```swift
/// Returns an array of (pid: pid_t, name: String, bundleID: String?)
public func enumerateAudioProcesses() -> [(pid_t, String, String?)]

/// Returns SCShareableContent for SCK-based capture
public func enumerateShareableContent() async throws -> SCShareableContent
```

## Implementation

### `enumerateAudioProcesses()`
1. Walk `NSWorkspace.runningApplications`
2. For each app, query `AudioObjectGetPropertyDataSize(kAudioHardwarePropertyProcessObjectList)` to check if it has an associated audio object
3. Return (PID, localized name, bundle ID) for apps with audio

### `enumerateShareableContent()`
1. Call `SCShareableContent.getAllShareableContent()` (async)
2. Filter to `.applications` with audio capability
3. Return for content picker UI or CLI listing

## Fallback Without Accessibility

If Accessibility permission is denied:
- Can still use `NSWorkspace.runningApplications` (no permission needed)
- Cannot correlate audio objects automatically
- User must supply PID manually via CLI

## Verification

- Run enumeration while multiple apps are playing audio
- Verify results include Chrome/Spotify/Zoom but not Finder
- Verify it works without Accessibility permission (with degraded results)

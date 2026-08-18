---
title: 02 — koe-native Swift Package
status: draft
depends: [01-workspace-setup]
spec_refs: [01-architecture, 07-native-bridge]
---

# 02 — koe-native Swift Package

Set up the Swift package that wraps macOS frameworks and exports a C ABI.

## Structure

```
koe-native/
  Package.swift
  Sources/
    AudioTap/
      AudioTap.swift
      RingBuffer.swift
    ScreenAudio/
      ScreenAudioCapture.swift
    SpeechAnalyzer/
      SpeechAnalyzerBridge.swift
    Permissions/
      PermissionChecker.swift
    AudioUtils/
      FormatNormalizer.swift
      ProcessEnumerator.swift
    StatusBar/
      StatusBarController.swift
  Tests/
    koe-nativeTests/
```

## Tasks

1. **Package.swift**
   - macOS 14+ deployment target
   - `.library(name: "koe-native", type: .dynamic)` — produce `.dylib`
   - Link system frameworks: `AudioToolbox`, `CoreAudio`, `AVFoundation`, `ScreenCaptureKit`, `Speech`, `ApplicationServices`

2. **Module organization**
   - Each subdirectory is a logical module
   - Public API surface: `AudioTap`, `ScreenAudioCapture`, `SpeechAnalyzerBridge`, `PermissionChecker`, `ProcessEnumerator`
   - Internal helpers: `RingBuffer`, `FormatNormalizer`

3. **Build verification**
   ```bash
   swift build -c release
   # produces: .build/arm64-apple-macosx/release/libkoe_native.dylib
   ```

4. **C ABI compatibility**
   - All types crossing to Rust are C-compatible (no Swift generics, no enums with associated values)
   - Use `@convention(c)` for callback function types
   - Memory owned by Swift is passed to Rust with explicit ownership semantics

## Verification

```bash
swift build
swift test
file .build/*/release/libkoe_native.dylib
# Should show: Mach-O 64-bit dynamically linked shared library arm64
```

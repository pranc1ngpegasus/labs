---
title: 03 — Permission Checker
status: draft
depends: [02-koe-native-package]
spec_refs: [06-permission-model, 07-native-bridge]
---

# 03 — Permission Checker

Implement permission status enumeration and checking in `koe-native`.

## Location

`koe-native/Sources/Permissions/PermissionChecker.swift`

## Data Types

```swift
public enum Permission {
    case microphone
    case screenRecording
    case accessibility
}

public enum PermissionStatus {
    case authorized
    case denied
    case restricted   // MDM / parental controls
    case notDetermined
}
```

## Implementation

1. **`checkPermission(_:) -> PermissionStatus`**
   - Microphone: `AVCaptureDevice.authorizationStatus(for: .audio)`
   - Screen Recording: `CGPreflightScreenCaptureAccess()` (lightweight check)
   - Accessibility: `AXIsProcessTrusted()`

2. **`requestMicrophonePermission() -> PermissionStatus`**
   - `AVCaptureDevice.requestAccess(for: .audio)` — async, triggers TCC dialog

3. **`allPermissionsStatus() -> [(Permission, PermissionStatus)]`**
   - Batch check for CLI `koe permissions` and GUI onboarding

## CLI Error Messages

When a permission is denied, provide actionable instructions including:
- Path to System Settings → Privacy & Security → [permission]
- Note that terminal app vs. GUI app have separate TCC entries
- Suggestion to use GUI for automatic prompt handling

## Verification

- Call `checkPermission(.microphone)` — must return correct status
- When denied, message must include the correct Settings path for the current macOS version

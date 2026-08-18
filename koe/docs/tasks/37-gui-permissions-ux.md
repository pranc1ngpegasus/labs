---
title: 37 — GUI Permissions UX
status: draft
depends: [31-gui-gpui-scaffold, 03-permission-checker]
spec_refs: [09-gui-interface, 06-permission-model]
---

# 37 — GUI Permissions Dialog

Implement the onboarding permissions flow and runtime permission checks.

## Location

`koe-gui/src/views/permissions.rs`

## Welcome/Onboarding Dialog

```
🖥️ Welcome to Koe

Koe needs 3 permissions to capture and transcribe audio:

  🎤 Microphone           [Authorize]
  🖥 Screen Recording      [Authorize]
  ♿ Accessibility         [Open Settings]

All audio stays on your device. Nothing is uploaded.
```

## Permission Actions

### Microphone
```rust
fn request_microphone(cx: &mut AppContext) {
    cx.spawn(|cx| async move {
        match koe_ffi::request_permission(Permission::Microphone) {
            PermissionStatus::Authorized => { /* update state */ }
            _ => { /* show instructions */ }
        }
    }).detach();
}
```
Triggers `AVCaptureDevice.requestAccess(for: .audio)` → system TCC dialog.

### Screen Recording
```rust
fn request_screen_recording(cx: &mut AppContext) {
    // CGRequestScreenCaptureAccess()
    // OR: just open the content picker (implicit trigger)
    cx.dispatch_action(ShowContentPicker);
}
```

### Accessibility
```rust
fn open_accessibility_settings() {
    // NSWorkspace.open(
    //   URL(string: "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")!
    // )
    koe_ffi::open_accessibility_settings();
}
```
Polls `AXIsProcessTrusted()` every 2 seconds until granted.

## Runtime Permission Checks

- Before starting a recording, check all required permissions
- If any are missing/denied, show inline banner (not modal):
  ```
  ⚠ Microphone permission required. [Grant Permission] [Open Settings]
  ```

## State Model

```rust
pub struct PermissionState {
    pub microphone: PermissionStatus,
    pub screen_recording: PermissionStatus,
    pub accessibility: PermissionStatus,
}

impl PermissionState {
    fn all_granted(&self) -> bool { /* ... */ }
    fn required_for_source(&self, source: &AudioSourceConfig) -> Vec<Permission> {
        match source {
            AudioSourceConfig::Microphone => vec![Permission::Microphone],
            AudioSourceConfig::AppAudio { .. } => vec![Permission::ScreenRecording, Permission::Accessibility],
            AudioSourceConfig::Both { .. } => vec![Permission::Microphone, Permission::ScreenRecording, Permission::Accessibility],
        }
    }
}
```

## Verification

- Fresh install → welcome dialog shows all three permissions
- Grant microphone → status updates to Authorized
- Deny screen recording → banner shows with instructions
- Grant accessibility after opening Settings → status updates on next poll
- All granted → welcome dialog dismissed

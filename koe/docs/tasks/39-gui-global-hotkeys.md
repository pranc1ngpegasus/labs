---
title: 39 — GUI Global Hotkeys
status: draft
depends: [31-gui-gpui-scaffold]
spec_refs: [09-gui-interface]
---

# 39 — Global Hotkey Registration

Register system-wide hotkeys for start/stop/pause.

## Location

`koe-gui/src/hotkey.rs`

## Hotkey Map

| Shortcut | Action |
|----------|--------|
| ⌘⇧R | Start recording from last-used source |
| ⌘⇧S | Stop recording |
| ⌘⇧P | Toggle pause/resume |

## Implementation

### Native Shim (koe-native)

```swift
// koe-native/Sources/Hotkey/HotkeyManager.swift
import Carbon

public final class HotkeyManager {
    public typealias HotkeyCallback = @convention(c) (Int32) -> Void

    private var hotkeys: [EventHotKeyRef] = []

    public func register(
        keyCode: UInt16,
        modifiers: UInt32,  // cmdKey | shiftKey
        id: Int32,
        callback: @escaping HotkeyCallback
    ) {
        var hotkey: EventHotKeyRef?
        let gEventHotKey: EventTypeSpec = EventTypeSpec(
            eventClass: OSType(kEventClassKeyboard),
            eventKind: UInt32(kEventHotKeyPressed)
        )

        RegisterEventHotKey(keyCode, modifiers, EventHotKeyID(signature: 0x4B4F4500, id: UInt32(id)),
            GetEventDispatcherTarget(), 0, &hotkey)

        InstallEventHandler(GetEventDispatcherTarget(), { _, event, _ in
            // Extract EventHotKeyID.id → call callback(id)
            callback(id)
            return noErr
        }, 1, &gEventHotKey, nil, nil)
    }

    public func unregisterAll() { /* UnregisterEventHotKey */ }
}
```

### Rust-Side (koe-gui)

```rust
use koe_ffi::HotkeyManager;

pub fn register_hotkeys(cx: &mut AppContext) {
    let tx = /* channel to send hotkey events to GPUI main thread */;

    cx.spawn(|mut cx| async move {
        let manager = HotkeyManager::new();

        // ⌘⇧R → id: 1
        manager.register(keycode(VK_R), cmd | shift, 1, move |id| {
            tx.send(id).unwrap();
        });

        // ⌘⇧S → id: 2
        manager.register(keycode(VK_S), cmd | shift, 2, move |id| { /* ... */ });

        // ⌘⇧P → id: 3
        manager.register(keycode(VK_P), cmd | shift, 3, move |id| { /* ... */ });

        // Listen on GPUI main thread
        while let Some(id) = rx.next().await {
            cx.update(|cx| {
                match id {
                    1 => cx.dispatch_action(RecordingAction::Start),
                    2 => cx.dispatch_action(RecordingAction::Stop),
                    3 => cx.dispatch_action(RecordingAction::TogglePause),
                    _ => {}
                }
            });
        }
    }).detach();
}
```

## Verification

- Register hotkeys at app launch
- Press ⌘⇧R → recording starts
- Press ⌘⇧P → recording pauses
- Press ⌘⇧S → recording stops
- Verify hotkeys work even when Koe is in the background

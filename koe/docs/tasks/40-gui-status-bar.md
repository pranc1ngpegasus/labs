---
title: 40 — GUI Status Bar / Menu Bar Item
status: draft
depends: [31-gui-gpui-scaffold]
spec_refs: [09-gui-interface]
---

# 40 — Menu Bar Item (v1 Stretch)

NSStatusBar item for quick recording control from the menu bar.

## Location

`koe-native/Sources/StatusBar/StatusBarController.swift`
(consumed by `koe-gui` via FFI)

## Visual Design

```
🎤 Koe (menu bar icon)
├── ⏺ Recording Chrome (00:34)
├── ⏹ Stop Recording
├── ⚙ Preferences…
└── ❌ Quit Koe
```

## Implementation (Native Shim)

```swift
public final class StatusBarController {
    private var statusItem: NSStatusItem?
    private var menu: NSMenu?

    public init(
        onStop: @escaping @convention(c) () -> Void,
        onPreferences: @escaping @convention(c) () -> Void,
        onQuit: @escaping @convention(c) () -> Void
    ) {
        statusItem = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        statusItem?.button?.title = "🎤"
        // Build menu with items that call the callbacks
    }

    public func updateStatus(text: String) {
        statusItem?.button?.title = "🎤 \(text)"
    }

    public func remove() {
        NSStatusBar.system.removeStatusItem(statusItem!)
    }
}
```

## Rust-Side (koe-gui)

```rust
pub fn setup_status_bar(pipeline: Model<RecordingPipeline>, cx: &mut AppContext) {
    let controller = StatusBarController::new(
        on_stop: { /* dispatch RecordingAction::Stop */ },
        on_preferences: { /* open Preferences window */ },
        on_quit: { /* cx.quit() */ },
    );

    // Periodically update status text from pipeline
    cx.spawn(|cx| async move {
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let text = format!("Recording Chrome ({})", format_elapsed(pipeline.elapsed()));
            controller.updateStatus(&text);
        }
    }).detach();
}
```

## Notes

- This is labeled "v1 Stretch" — implement only if GPUI integration is smooth
- GPUI does not natively expose NSStatusBar APIs, so a native shim is required
- Menu bar item provides quick access without bringing the main window into focus

## Verification

- App launches → menu bar icon appears
- Recording starts → menu bar shows status
- Click "Stop Recording" in menu → recording stops
- Click "Preferences" → preferences window opens
- Click "Quit Koe" → app exits

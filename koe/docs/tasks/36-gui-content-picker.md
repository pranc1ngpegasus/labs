---
title: 36 — GUI Content Picker
status: draft
depends: [31-gui-gpui-scaffold, 08-screen-capture-kit-capture]
spec_refs: [09-gui-interface, 03-screen-capture-kit]
---

# 36 — GUI SCK Content Picker Integration

Integrate system and custom content pickers for selecting capture targets.

## Location

`koe-gui/src/views/content_picker.rs`

## Implementation

### macOS 14+: SCContentSharingPicker (System-standard)

```swift
// koe-native/Sources/ScreenAudio/ContentPicker.swift
public func presentSystemPicker() async throws -> SCShareableContent {
    let picker = SCContentSharingPicker()
    picker.isAudioIncluded = true
    picker.present()
    // Returns via delegate didPickContent:
}
```

### macOS 12–13: Custom Picker

When `SCContentSharingPicker` is unavailable, implement a custom modal:

```rust
// koe-gui/src/views/content_picker.rs
pub struct CustomContentPicker {
    content: Model<Vec<AppInfo>>,
    selected: Model<Option<AppInfo>>,
    search_query: Model<String>,
}

impl Render for CustomContentPicker {
    fn render(&mut self, cx: &mut ViewContext<Self>) -> impl IntoElement {
        div().flex().flex_col().p_4().gap_2()
            // Search bar
            .child(TextInput::new(cx, self.search_query.clone()))
            // App list
            .child(List::new(cx, self.content.clone(), |item| {
                h_stack().gap_2().children(
                    // App icon
                    Label::new(item.name.clone()),
                    Label::new(item.bundle_id.clone().unwrap_or_default())
                        .color(rgb(0x666666)),
                )
            }))
            // Confirm/Cancel buttons
            .child(h_stack().gap_2().children(
                Button::new("Cancel").on_click(|cx| cx.dispatch_action(CloseContentPicker)),
                Button::new("Select").on_click(|cx| cx.dispatch_action(ConfirmContentPicker)),
            ))
    }
}
```

## Integration Flow

1. User clicks [Change Source] in Source Panel
2. GPUI dispatches `ShowContentPicker` action
3. macOS 14+: calls native `presentSystemPicker()`
4. macOS 12–13: opens custom modal with `enumerateShareableContent()` results
5. Selection → restart capture with new target

## Verification

- Open picker → list of audio-capable apps appears
- Search/filter reduces list
- Select app → capture starts on that app
- Cancel → no change to current capture
- Test on both macOS 14+ and 13 (if available)

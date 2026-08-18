---
title: 33 — GUI Source Panel
status: draft
depends: [31-gui-gpui-scaffold, 06-process-enumeration]
spec_refs: [09-gui-interface]
---

# 33 — GUI Source Selection Panel

Capture source display and selection UI.

## Location

`koe-gui/src/views/source_panel.rs`

## Visual Design

```
● Google Chrome
  Audio: ●●●●
Microphone: Built-in
  Audio: ●●
[Change Source]
```

## Components

1. **Active source display**
   - Icon + app name for system audio source
   - Input device name for microphone source
   - Per-source level indicators (small bars or dots)

2. **Change Source button**
   - Opens content picker (Task 36) for SCK sources
   - Or presents mic device selection

3. **Source state**
   - Idle: "No source selected"
   - Capturing: app name + level indicator
   - Error: "App not available" (if target quit)

## Implementation

```rust
pub struct SourcePanel {
    source_model: Model<SourceState>,
}

pub struct SourceState {
    pub app_name: String,
    pub app_icon: Option<AppIcon>,
    pub audio_level: f32,
    pub mic_name: Option<String>,
    pub mic_level: f32,
    pub is_capturing: bool,
}

impl Render for SourcePanel {
    fn render(&mut self, cx: &mut ViewContext<Self>) -> impl IntoElement {
        let source = self.source_model.read(cx);

        div().flex().flex_col().p_4().gap_2().children(
            // App source
            h_stack().gap_2().children(
                // Icon placeholder
                div().size(px(24.)).bg(rgb(0x444444)).rounded(px(4.)),
                Label::new(source.app_name.clone()).size(px(14.)),
            ),
            // Level indicator
            div().flex().children(
                // Small level bar
            ),
        )
    }
}
```

## Interaction

- Click [Change Source] → emit `ShowContentPicker` action
- Content picker callback → update source state
- Source state change → restart capture

## Verification

- Display active source with name and level
- Click change source → picker opens
- Pick new source → capture restarts
- Target quits → error state displayed

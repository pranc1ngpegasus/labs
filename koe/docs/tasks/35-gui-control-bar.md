---
title: 35 — GUI Control Bar (Transport)
status: draft
depends: [31-gui-gpui-scaffold, 15-pipeline-core]
spec_refs: [09-gui-interface]
---

# 35 — GUI Transport Control Bar

Recording controls: record, pause, stop with status display.

## Location

`koe-gui/src/views/control_bar.rs`

## Visual Design

```
⏺ Recording | 00:02:34 | OGG 48kHz | 7.8 MB    [⏸ Pause] [⏹ Stop & Save]
```

## States

| State | Status Text | Buttons |
|-------|------------|---------|
| Idle | "Ready" | [⏺ Record] |
| Recording | "⏺ Recording \| 00:02:34 \| OGG 48kHz \| 7.8 MB" | [⏸ Pause] [⏹ Stop] |
| Paused | "⏸ Paused \| 00:02:34" | [▶ Resume] [⏹ Stop] |
| Stopping | "Stopping..." | (disabled) |

## Implementation

```rust
pub struct ControlBar {
    recording_model: Model<RecordingState>,
}

#[derive(Clone)]
pub enum RecordingAction {
    Start,
    Pause,
    Resume,
    Stop,
}

impl Render for ControlBar {
    fn render(&mut self, cx: &mut ViewContext<Self>) -> impl IntoElement {
        let state = self.recording_model.read(cx);

        div().flex().items_center().justify_between().p_4()
            .bg(rgb(0x222222))
            .children(
                // Left: status
                match state {
                    RecordingState::Idle => Label::new("Ready"),
                    RecordingState::Recording { elapsed, bytes, format } =>
                        Label::new(format!(
                            "⏺ Recording | {} | {} {}kHz | {} MB",
                            format_duration(elapsed),
                            format.name(),
                            format.sample_rate() / 1000,
                            bytes / 1_000_000,
                        )),
                    RecordingState::Paused { elapsed } =>
                        Label::new(format!("⏸ Paused | {}", format_duration(elapsed))),
                    RecordingState::Stopping => Label::new("Stopping..."),
                    RecordingState::Stopped => Label::new("Recording saved"),
                },
                // Right: buttons
                h_stack().gap_2().children(
                    match state {
                        RecordingState::Idle =>
                            button("⏺ Record").on_click(|cx| cx.dispatch_action(RecordingAction::Start)),
                        RecordingState::Recording { .. } => [
                            button("⏸ Pause").on_click(|cx| cx.dispatch_action(RecordingAction::Pause)),
                            button("⏹ Stop & Save").on_click(|cx| cx.dispatch_action(RecordingAction::Stop)),
                        ],
                        RecordingState::Paused { .. } => [
                            button("▶ Resume").on_click(|cx| cx.dispatch_action(RecordingAction::Resume)),
                            button("⏹ Stop & Save").on_click(|cx| cx.dispatch_action(RecordingAction::Stop)),
                        ],
                        _ => [],
                    }
                ),
            )
    }
}
```

## Status Update Loop

- Pipeline publishes status via broadcast channel at ~10 Hz
- ControlBar subscribes and updates the model

## Keyboard Shortcuts

| Shortcut | Action |
|----------|--------|
| ⌘⇧R | Start recording |
| ⌘⇧S | Stop recording |
| ⌘⇧P | Toggle pause |

## Verification

- Idle → click Record → state transitions to Recording
- Recording → click Pause → state transitions to Paused
- Paused → click Resume → state transitions back to Recording
- Recording → click Stop → state transitions to Stopping → Stopped
- Verify elapsed time and bytes counters update in real time

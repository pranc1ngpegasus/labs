---
title: 34 — GUI Live Transcript View
status: draft
depends: [31-gui-gpui-scaffold, 20-transcript-formatter]
spec_refs: [09-gui-interface, 04-speech-recognition]
---

# 34 — GUI Live Transcript View

Virtual-scrolling transcript with partial and final segments.

## Location

`koe-gui/src/views/transcript.rs`

## Visual Design

```
[00:01:23] This is a finalized segment of
           transcribed speech.
[00:01:28] This is a partial result still being…
           (italic, gray)
```

- `UniformList` for virtual scrolling
- Finalized segments: standard font, normal weight, white text
- Partial segments: italic, gray text
- Timestamps left-aligned
- Auto-scroll to latest segment

## Segment Row

```rust
struct TranscriptRow {
    pub start_ms: i64,
    pub end_ms: Option<i64>,
    pub text: String,
    pub is_final: bool,
    pub confidence: f32,
}

impl Render for TranscriptRow {
    fn render(&mut self, cx: &mut ViewContext<Self>) -> impl IntoElement {
        let text_style = if self.is_final {
            TextStyle::default() // white, regular
        } else {
            TextStyle {
                color: Some(rgb(0x888888)),
                italic: true,
                ..Default::default()
            }
        };

        h_stack().gap_2().children(
            // Timestamp
            Label::new(format_timestamp(self.start_ms))
                .size(px(11.))
                .color(rgb(0x666666)),
            // Text
            Label::new(self.text.clone())
                .style(text_style)
                .size(px(13.)),
        )
    }
}
```

## Auto-Scroll

```rust
fn scroll_to_latest(&mut self, cx: &mut ViewContext<Self>) {
    let count = self.model.read(cx).segments.len();
    if count > 0 {
        self.list.scroll_to_reveal_item(count - 1);
    }
}
```

## Update from Pipeline

```rust
// In MainView or AppModel:
fn on_transcription_segment(&mut self, segment: TranscriptionSegment, cx: &mut ModelContext<Self>) {
    self.segments.update(cx, |segments, cx| {
        if segment.is_final {
            // Replace any matching partial segment or append
            if let Some(existing) = segments.iter_mut().find(|s| !s.is_final) {
                *existing = TranscriptRow::from(segment);
            } else {
                segments.push(TranscriptRow::from(segment));
            }
        } else {
            // Update or add partial segment
            if let Some(existing) = segments.iter_mut().last() {
                if !existing.is_final {
                    *existing = TranscriptRow::from(segment);
                    cx.notify();
                    return;
                }
            }
            segments.push(TranscriptRow::from(segment));
        }
        cx.notify();
    });
}
```

## Verification

- Feed partial segments → italic gray text appears
- Feed final segment → snaps to normal style
- Scroll with many segments → virtual scrolling works
- New segments → auto-scrolls to bottom

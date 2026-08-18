---
title: 32 — GUI Audio Level Meters
status: draft
depends: [31-gui-gpui-scaffold, 15-pipeline-core]
spec_refs: [09-gui-interface]
---

# 32 — GUI Audio Level Meters

GPU-rendered stereo level meter bars using GPUI Canvas.

## Location

`koe-gui/src/views/meters.rs`

## Visual Design

```
(L) ▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓
(R) ▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓
```

- Two vertical or horizontal bars (L/R channels)
- Filled from left-to-right, color gradient (green → yellow → red)
- 60 fps update driven by audio level data from pipeline

## Data Flow

```
koe-core pipeline
  → tokio::broadcast (audio levels, ~60 Hz)
    → GPUI background executor
      → Model::update() → cx.notify()
        → Canvas::paint() reads model.levels: Vec<f32>
```

## Implementation

```rust
pub struct AudioMeters {
    level_model: Model<AudioLevels>,
}

impl Render for AudioMeters {
    fn render(&mut self, cx: &mut ViewContext<Self>) -> impl IntoElement {
        Canvas::new()
            .size(size(px(200.), px(20.)))
            .paint(move |bounds, cx| {
                let levels = self.level_model.read(cx);

                // Background (dark)
                let bg_color = rgb(0x333333);
                cx.fill(bounds, bg_color);

                // L channel bar
                let l_width = bounds.size.width * levels.left;
                cx.fill(
                    Bounds::new(bounds.origin, size(l_width, bounds.size.height / 2.0)),
                    level_color(levels.left),
                );

                // R channel bar
                let r_width = bounds.size.width * levels.right;
                let r_origin = point(bounds.origin.x, bounds.origin.y + bounds.size.height / 2.0);
                cx.fill(
                    Bounds::new(r_origin, size(r_width, bounds.size.height / 2.0)),
                    level_color(levels.right),
                );
            })
    }
}

fn level_color(level: f32) -> Hsla {
    match level {
        x if x < 0.5 => rgb(0x00AA00),   // Green
        x if x < 0.8 => rgb(0xAAAA00),   // Yellow
        _ => rgb(0xAA0000),              // Red
    }
}
```

## Update Loop

```rust
impl AppModel {
    fn subscribe_levels(&mut self, cx: &mut ModelContext<Self>) {
        let mut rx = self.pipeline.subscribe_levels();
        cx.background_executor()
            .spawn(async move {
                while let Ok(levels) = rx.recv().await {
                    self.levels.update(cx, |model, cx| {
                        model.left = levels.left;
                        model.right = levels.right;
                        cx.notify();
                    });
                }
            })
            .detach();
    }
}
```

## Verification

- Start recording, verify meters respond to audio level
- Verify 60 fps rendering (no jank)
- Test with silence → meters at minimum
- Test with loud audio → meters at maximum (red)

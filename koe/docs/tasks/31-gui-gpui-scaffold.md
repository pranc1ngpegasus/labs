---
title: 31 — GUI GPUI App Scaffold
status: draft
depends: [01-workspace-setup]
spec_refs: [09-gui-interface]
---

# 31 — GUI GPUI Application Scaffold

Set up the GPUI-based GUI application shell.

## Location

`koe-gui/` crate

## Dependencies

```toml
[dependencies]
gpui = { git = "https://github.com/zed-industries/zed", rev = "<pinned-rev>" }
koe-core = { path = "../koe-core" }
koe-ffi = { path = "../koe-ffi" }
tokio = ...
```

## App Structure

```rust
// koe-gui/src/main.rs
fn main() {
    App::new().run(|cx: &mut AppContext| {
        // 1. Load settings
        let settings = Settings::load();

        // 2. Register actions
        actions::register(cx);

        // 3. Open main window
        cx.open_window(
            WindowOptions {
                titlebar: Some(TitlebarOptions {
                    title: Some("Koe".into()),
                    appears_transparent: true,
                    traffic_light_position: Some(point(px(10.), px(10.))),
                }),
                bounds: Some(Bounds::centered(None, size(px(800.), px(600.)), cx)),
                ..Default::default()
            },
            |cx| MainView::new(cx, settings),
        );

        // 4. Activate window
        cx.activate(true);
    });
}
```

## MainView

```rust
pub struct MainView {
    settings: Model<Settings>,
    pipeline: Model<Option<RecordingPipeline>>,
    levels: Model<AudioLevels>,
    segments: Model<Vec<TranscriptionSegment>>,
    recording_state: Model<RecordingState>,
}

impl Render for MainView {
    fn render(&mut self, cx: &mut ViewContext<Self>) -> impl IntoElement {
        div().flex().flex_col().size_full().children(
            // Audio Meters (Task 32)
            // Source Panel (Task 33)
            // Live Transcript (Task 34)
            // Control Bar (Task 35)
        )
    }
}
```

## GPUI Version Strategy

- Pin a known-good Zed release tag
- Test on each macOS major release
- Upgrade only when new Zed stable is available and tested

## Features Used
- `gpui::Window` — NSWindow shell
- `gpui::Canvas` — Metal-backed custom rendering
- `gpui::UniformList` — virtual scrolling
- `gpui::text` — text layout
- `gpui::actions` — keyboard shortcut dispatch
- `gpui::px` / `gpui::rem` — DPI-aware measurement

## Verification

```bash
cargo build -p koe-gui --features gui
# Opens a blank GPUI window with title "Koe"
```

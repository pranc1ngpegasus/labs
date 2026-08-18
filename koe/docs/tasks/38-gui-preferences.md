---
title: 38 — GUI Preferences Window
status: draft
depends: [31-gui-gpui-scaffold]
spec_refs: [09-gui-interface]
---

# 38 — GUI Preferences Window

Settings window with General, Audio, and Shortcuts tabs.

## Location

`koe-gui/src/views/preferences.rs`

## Tabs

### General
```
Default Locale:        [English (US)                ▼]
Output Directory:      [~/Recordings/Koe          ...]
Audio Format:          [OGG ▼] [48kHz ▼]
Transcript Format:     [SRT ▼]
```

### Audio
```
[✓] Echo Cancellation
[✓] Comfort Noise
```

### Shortcuts
```
Start Recording:  [⌘⇧R]
Stop Recording:   [⌘⇧S]
Pause/Resume:     [⌘⇧P]
```

## Implementation

```rust
pub struct PreferencesView {
    settings: Model<Settings>,
    active_tab: Model<PrefsTab>,
}

pub enum PrefsTab { General, Audio, Shortcuts }

impl Render for PreferencesView {
    fn render(&mut self, cx: &mut ViewContext<Self>) -> impl IntoElement {
        div().flex().flex_row().size_full()
            // Tab sidebar
            .child(
                div().flex().flex_col().w(px(150.)).p_4().gap_1().children(
                    [PrefsTab::General, PrefsTab::Audio, PrefsTab::Shortcuts]
                        .iter()
                        .map(|tab| tab_button(tab, self.active_tab.clone()))
                )
            )
            // Content area
            .child(match self.active_tab.read(cx) {
                PrefsTab::General => self.render_general(cx),
                PrefsTab::Audio => self.render_audio(cx),
                PrefsTab::Shortcuts => self.render_shortcuts(cx),
            })
    }
}
```

## Settings Persistence

```rust
pub struct Settings {
    pub locale: String,              // "en-US"
    pub output_directory: PathBuf,   // "~/Recordings/Koe"
    pub audio_format: OutputFormat,  // Ogg { quality: 0.4 }
    pub transcript_format: TranscriptFormat,
    pub aec_enabled: bool,
    pub comfort_noise: bool,
    pub shortcuts: HashMap<String, KeyBinding>,
}

impl Settings {
    pub fn load() -> Self { /* from ~/Library/Application Support/Koe/settings.json */ }
    pub fn save(&self) { /* write JSON */ }
}
```

## Form Components Used
- `Dropdown` — locale, format selection (custom or built via GPUI primitives)
- `TextInput` — output directory with "..." browse button
- `Checkbox` — boolean toggles
- `KeyBinding` — shortcut recording (capture next key combo)

## Verification

- Open Preferences → General tab visible
- Change locale → saved to settings.json
- Toggle AEC checkbox → saved
- Switch tabs → correct content renders
- Close window → settings persisted

---
title: 41 — GUI Theme System
status: draft
depends: [31-gui-gpui-scaffold]
spec_refs: [09-gui-interface]
---

# 41 — GUI Theme System

Light and dark theme wrapper over GPUI's theme primitives.

## Location

`koe-gui/src/theme.rs`

## Rationale

GPUI's theme system is designed for code editors (syntax highlighting, etc.).
Koe provides a simplified wrapper with light/dark variants.

## Theme Definition

```rust
pub struct KoeTheme {
    pub name: String,
    pub is_dark: bool,

    // Backgrounds
    pub bg_primary: Hsla,
    pub bg_secondary: Hsla,
    pub bg_transcript: Hsla,

    // Text
    pub text_primary: Hsla,
    pub text_secondary: Hsla,
    pub text_muted: Hsla,
    pub text_partial: Hsla,      // For partial transcript segments

    // Accent
    pub accent: Hsla,
    pub accent_hover: Hsla,

    // Level meters
    pub level_green: Hsla,
    pub level_yellow: Hsla,
    pub level_red: Hsla,

    // Status
    pub recording_red: Hsla,
    pub paused_yellow: Hsla,
    pub error_red: Hsla,

    // Controls
    pub button_bg: Hsla,
    pub button_hover: Hsla,
    pub border: Hsla,
}
```

## Built-in Themes

### Light
```rust
pub fn light_theme() -> KoeTheme {
    KoeTheme {
        name: "Light".into(),
        is_dark: false,
        bg_primary: rgb(0xFFFFFF),
        bg_secondary: rgb(0xF5F5F5),
        bg_transcript: rgb(0xFAFAFA),
        text_primary: rgb(0x1A1A1A),
        text_secondary: rgb(0x666666),
        text_muted: rgb(0x999999),
        text_partial: rgb(0xAAAAAA),
        accent: rgb(0x007AFF),
        level_green: rgb(0x34C759),
        level_yellow: rgb(0xFFCC00),
        level_red: rgb(0xFF3B30),
        recording_red: rgb(0xFF3B30),
        paused_yellow: rgb(0xFFCC00),
        // ...
    }
}
```

### Dark (Default)
```rust
pub fn dark_theme() -> KoeTheme {
    KoeTheme {
        name: "Dark".into(),
        is_dark: true,
        bg_primary: rgb(0x1E1E1E),
        bg_secondary: rgb(0x252525),
        bg_transcript: rgb(0x1A1A1A),
        text_primary: rgb(0xE0E0E0),
        text_secondary: rgb(0x999999),
        text_muted: rgb(0x666666),
        text_partial: rgb(0x555555),
        // ...
    }
}
```

## Usage

```rust
// Theme is stored in Global model
cx.set_global(KoeTheme::dark());

// Views access via:
fn render(&mut self, cx: &mut ViewContext<Self>) -> impl IntoElement {
    let theme = cx.global::<KoeTheme>();
    div().bg(theme.bg_primary).text_color(theme.text_primary)
}
```

## System Theme Detection

```rust
pub fn detect_system_theme() -> KoeTheme {
    // NSApp.effectiveAppearance.name == .darkAqua
    if is_dark_mode() { dark_theme() } else { light_theme() }
}
```

## Verification

- App launches in dark theme (default)
- Switch macOS to light mode → app follows
- Verify all text is readable in both themes
- Verify level meter colors are visible in both themes
- Verify partial transcript segments are distinguishable from final

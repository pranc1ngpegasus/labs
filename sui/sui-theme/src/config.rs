//! Loading named themes from `config.toml`.
//!
//! The active theme is resolved from a TOML document (by default
//! `$XDG_CONFIG_HOME/sui/config.toml` or `~/.config/sui/config.toml`):
//!
//! ```toml
//! [theme]
//! active = "foo"
//!
//! [theme.foo]
//! prompt_border = "cyan"
//! shell_border = "#ff00ff"
//! prompt_background = "#808080"
//! selection_fg = "black"
//! selection_bg = "yellow"
//! ```
//!
//! `theme.default` is reserved for the hard-coded built-in palette
//! ([`Theme::DEFAULT`]); it cannot be overridden. Any colour that fails to
//! parse — and any unknown or malformed theme — falls back to the default.

use crate::Theme;
use ratatui::style::Color;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;

/// Parsed theme selection: the active theme name plus the named theme table.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    theme: ThemeSection,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct ThemeSection {
    /// Name of the theme to apply; `"default"` (or absence) selects the
    /// built-in palette. Unknown names fall back to the default.
    active: Option<String>,
    /// Named themes declared as `[theme.<name>]`. The name `default` is
    /// reserved and ignored.
    #[serde(flatten)]
    themes: HashMap<String, ThemeConfig>,
}

/// Colour overrides for one named theme. Missing fields inherit the default.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "snake_case")]
struct ThemeConfig {
    prompt_border: Option<String>,
    shell_border: Option<String>,
    prompt_background: Option<String>,
    selection_fg: Option<String>,
    selection_bg: Option<String>,
}

impl Config {
    /// Parses a full config document. Malformed TOML yields a default `Config`
    /// (so the active theme degrades to [`Theme::DEFAULT`]).
    #[must_use]
    pub fn parse(input: &str) -> Self {
        toml::from_str(input).unwrap_or_default()
    }

    /// Resolves the active theme, falling back to [`Theme::DEFAULT`] for the
    /// reserved `default` name, an unknown name, or unparseable colours.
    #[must_use]
    pub fn active_theme(&self) -> Theme {
        const RESERVED: &str = "default";
        let name = self.theme.active.as_deref().unwrap_or(RESERVED);
        // `default` is reserved: it can never be overridden by a table.
        if name == RESERVED {
            return Theme::DEFAULT;
        }
        self.theme
            .themes
            .get(name)
            .cloned()
            .unwrap_or_default()
            .apply(Theme::DEFAULT)
    }
}

impl ThemeConfig {
    fn apply(
        self,
        base: Theme,
    ) -> Theme {
        Theme {
            prompt_border: pick(base.prompt_border, self.prompt_border.as_deref()),
            shell_border: pick(base.shell_border, self.shell_border.as_deref()),
            prompt_background: pick(base.prompt_background, self.prompt_background.as_deref()),
            selection_fg: pick(base.selection_fg, self.selection_fg.as_deref()),
            selection_bg: pick(base.selection_bg, self.selection_bg.as_deref()),
        }
    }
}

fn pick(
    default: Color,
    spec: Option<&str>,
) -> Color {
    spec.and_then(parse_color).unwrap_or(default)
}

/// Parses a colour from a hex string (`#rgb` / `#rrggbb`) or a common name.
#[must_use]
fn parse_color(input: &str) -> Option<Color> {
    let input = input.trim();
    if let Some(hex) = input.strip_prefix('#') {
        return parse_hex(hex);
    }
    parse_name(input)
}

fn parse_hex(hex: &str) -> Option<Color> {
    // Input is validated as hex below; byte slicing is safe because hex is ASCII.
    match hex.len() {
        3 => {
            let r = hex_byte(&hex[0..1])?;
            let g = hex_byte(&hex[1..2])?;
            let b = hex_byte(&hex[2..3])?;
            Some(Color::Rgb(r * 17, g * 17, b * 17))
        },
        6 => {
            let r = hex_byte(&hex[0..2])?;
            let g = hex_byte(&hex[2..4])?;
            let b = hex_byte(&hex[4..6])?;
            Some(Color::Rgb(r, g, b))
        },
        _ => None,
    }
}

fn hex_byte(s: &str) -> Option<u8> {
    u8::from_str_radix(s, 16).ok()
}

fn parse_name(name: &str) -> Option<Color> {
    let color = match name.to_ascii_lowercase().as_str() {
        "black" => Color::Black,
        "red" => Color::Red,
        "green" => Color::Green,
        "yellow" => Color::Yellow,
        "blue" => Color::Blue,
        "magenta" => Color::Magenta,
        "cyan" => Color::Cyan,
        "gray" | "grey" => Color::Gray,
        "darkgray" | "darkgrey" => Color::DarkGray,
        "white" => Color::White,
        "lightred" => Color::LightRed,
        "lightgreen" => Color::LightGreen,
        "lightyellow" => Color::LightYellow,
        "lightblue" => Color::LightBlue,
        "lightmagenta" => Color::LightMagenta,
        "lightcyan" => Color::LightCyan,
        "reset" => Color::Reset,
        _ => return None,
    };
    Some(color)
}

/// Default `config.toml` location, honouring `$SUI_CONFIG` and `$XDG_CONFIG_HOME`.
#[must_use]
pub fn default_config_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("SUI_CONFIG").filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(path));
    }
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(base.join("sui").join("config.toml"))
}

/// Loads the active theme from the default `config.toml`, if present.
///
/// A missing file silently yields [`Theme::DEFAULT`]. A malformed document (or
/// an unreadable file) logs a warning and also yields [`Theme::DEFAULT`].
#[must_use]
pub fn load_active() -> Theme {
    let Some(path) = default_config_path() else {
        return Theme::DEFAULT;
    };
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Theme::DEFAULT;
    };
    match toml::from_str::<Config>(&raw) {
        Ok(config) => config.active_theme(),
        Err(error) => {
            eprintln!(
                "sui: ignoring malformed theme config at {}: {error}",
                path.display()
            );
            Theme::DEFAULT
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_default_resolves_builtin() {
        let config = Config::parse("[theme]\nactive = \"default\"\n");
        assert_eq!(config.active_theme(), Theme::DEFAULT);
    }

    #[test]
    fn parse_missing_active_resolves_builtin() {
        assert_eq!(Config::parse("").active_theme(), Theme::DEFAULT);
    }

    #[test]
    fn parse_named_theme_applies_all_colours() {
        let config = Config::parse(
            r##"
            [theme]
            active = "foo"
            [theme.foo]
            prompt_border = "red"
            shell_border = "#00ff00"
            prompt_background = "#808080"
            selection_fg = "black"
            selection_bg = "#ffff00"
            "##,
        );
        let theme = config.active_theme();
        assert_eq!(theme.prompt_border, Color::Red);
        assert_eq!(theme.shell_border, Color::Rgb(0, 255, 0));
        assert_eq!(theme.prompt_background, Color::Rgb(128, 128, 128));
        assert_eq!(theme.selection_fg, Color::Black);
        assert_eq!(theme.selection_bg, Color::Rgb(255, 255, 0));
    }

    #[test]
    fn parse_partial_theme_inherits_unset_fields() {
        let config = Config::parse(
            r#"
            [theme]
            active = "solo"
            [theme.solo]
            prompt_border = "magenta"
            "#,
        );
        let theme = config.active_theme();
        assert_eq!(theme.prompt_border, Color::Magenta);
        assert_eq!(theme.shell_border, Theme::DEFAULT.shell_border);
    }

    #[test]
    fn parse_bad_colour_falls_back_to_default() {
        let config = Config::parse(
            r##"
            [theme]
            active = "odd"
            [theme.odd]
            prompt_border = "not-a-colour"
            shell_border = "#12"
            "##,
        );
        let theme = config.active_theme();
        assert_eq!(theme.prompt_border, Theme::DEFAULT.prompt_border);
        assert_eq!(theme.shell_border, Theme::DEFAULT.shell_border);
    }

    #[test]
    fn parse_unknown_active_theme_falls_back() {
        let config =
            Config::parse("[theme]\nactive = \"nope\"\n[theme.solo]\nprompt_border = \"red\"\n");
        assert_eq!(config.active_theme(), Theme::DEFAULT);
    }

    #[test]
    fn default_theme_is_reserved_and_cannot_be_overridden() {
        let config = Config::parse(
            r#"
            [theme]
            active = "default"
            [theme.default]
            prompt_border = "red"
            "#,
        );
        assert_eq!(config.active_theme(), Theme::DEFAULT);
    }

    #[test]
    fn parse_malformed_toml_yields_default() {
        assert_eq!(
            Config::parse("this is not [valid toml").active_theme(),
            Theme::DEFAULT
        );
    }

    #[test]
    fn parse_hex_short_and_long() {
        assert_eq!(parse_color("#f00"), Some(Color::Rgb(255, 0, 0)));
        assert_eq!(parse_color("#336699"), Some(Color::Rgb(0x33, 0x66, 0x99)));
        assert_eq!(parse_color("nope"), None);
        assert_eq!(parse_color("#zzz"), None);
    }

    #[test]
    fn parse_names_are_case_insensitive() {
        assert_eq!(parse_color("CYAN"), Some(Color::Cyan));
        assert_eq!(parse_color("DarkGray"), Some(Color::DarkGray));
        assert_eq!(parse_color("GREY"), Some(Color::Gray));
    }
}

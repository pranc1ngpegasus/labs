//! TOML config loading and path helpers for `koe-cli`.
//!
//! Precedence: CLI flags > config file > built-in defaults.
//! Default path: `~/.config/koe/config.toml` (explicit XDG layout under `$HOME`).

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Built-in defaults when neither CLI nor config sets a value.
pub mod builtin {
    pub const SOURCE: &str = "system";
    pub const FORMAT: &str = "ogg";
    pub const LOCALE: &str = "en-US";
    pub const TRANSCRIPT_FORMAT: &str = "txt";
    pub const SPEECH_ENGINE: &str = "auto";
    pub const SAMPLE_RATE_HZ: u32 = 48_000;
    pub const CHANNELS: u8 = 2;
    pub const AEC_ENABLED: bool = true;
    pub const COMFORT_NOISE: bool = true;
}

/// Root config file shape (`~/.config/koe/config.toml`).
#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct KoeConfig {
    pub defaults: DefaultsSection,
    pub aec: AecSection,
    pub output: OutputSection,
    pub transcription: TranscriptionSection,
}

/// Defaults applied mainly by `koe record`.
#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
pub struct DefaultsSection {
    pub source: Option<String>,
    pub format: Option<String>,
    pub locale: Option<String>,
    pub transcript_format: Option<String>,
    pub engine: Option<String>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u8>,
}

/// Acoustic echo cancellation defaults for `koe record --source both`.
#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
pub struct AecSection {
    pub enabled: Option<bool>,
    pub comfort_noise: Option<bool>,
}

/// Default output directory (tilde-expanded when used).
#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OutputSection {
    pub directory: Option<String>,
}

/// Defaults for `koe transcribe` (falls back to [`DefaultsSection`] when unset).
#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
pub struct TranscriptionSection {
    pub locale: Option<String>,
    pub transcript_format: Option<String>,
    pub engine: Option<String>,
}

/// Loads config from `--config` or the default path.
///
/// - Missing default path → empty [`KoeConfig`] (all built-in defaults).
/// - Missing explicit `--config` path → error.
pub fn load(explicit: Option<&Path>) -> Result<KoeConfig, String> {
    let (path, required) = match explicit {
        Some(path) => (expand_tilde_path(path), true),
        None => match default_path() {
            Some(path) => (path, false),
            None => return Ok(KoeConfig::default()),
        },
    };

    if !path.exists() {
        if required {
            return Err(format!("config file not found: {}", path.display()));
        }
        return Ok(KoeConfig::default());
    }

    let text = std::fs::read_to_string(&path)
        .map_err(|err| format!("failed to read config {}: {err}", path.display()))?;
    parse_toml(&text).map_err(|err| format!("invalid config {}: {err}", path.display()))
}

/// Parses a TOML document into [`KoeConfig`].
pub fn parse_toml(text: &str) -> Result<KoeConfig, String> {
    toml::from_str(text).map_err(|err| err.to_string())
}

/// `~/.config/koe/config.toml` when `$HOME` is set.
#[must_use]
pub fn default_path() -> Option<PathBuf> {
    home_dir().map(|home| home.join(".config/koe/config.toml"))
}

/// Expands a leading `~` / `~/` using `$HOME`. Other paths are unchanged.
#[must_use]
pub fn expand_tilde(path: &str) -> PathBuf {
    if path == "~" {
        return home_dir().unwrap_or_else(|| PathBuf::from("~"));
    }
    if let Some(rest) = path.strip_prefix("~/")
        && let Some(home) = home_dir()
    {
        return home.join(rest);
    }
    PathBuf::from(path)
}

/// Expands `~` when the path is UTF-8; otherwise returns `path` unchanged.
#[must_use]
pub fn expand_tilde_path(path: &Path) -> PathBuf {
    path.to_str()
        .map_or_else(|| path.to_path_buf(), expand_tilde)
}

/// Resolved output directory from config, if any.
#[must_use]
pub fn output_directory(config: &KoeConfig) -> Option<PathBuf> {
    config.output.directory.as_deref().map(expand_tilde)
}

/// Joins a relative output path onto the configured directory.
///
/// Absolute paths and paths without a configured directory are returned as-is.
#[must_use]
pub fn resolve_under_output_dir(
    output: &Path,
    config: &KoeConfig,
) -> PathBuf {
    if output.is_absolute() {
        return output.to_path_buf();
    }
    output_directory(config).map_or_else(|| output.to_path_buf(), |dir| dir.join(output))
}

/// CLI `Option` overrides config `Option`, then built-in default.
#[must_use]
pub fn coalesce_owned(
    cli: Option<String>,
    config: Option<&str>,
    default: &str,
) -> String {
    cli.or_else(|| config.map(str::to_owned))
        .unwrap_or_else(|| default.to_owned())
}

/// Same as [`coalesce_owned`] for `Copy` values.
#[must_use]
pub fn coalesce_copy<T: Copy>(
    cli: Option<T>,
    config: Option<T>,
    default: T,
) -> T {
    cli.or(config).unwrap_or(default)
}

/// Locale for `koe transcribe`: CLI > `[transcription]` > `[defaults]` > built-in.
#[must_use]
pub fn transcribe_locale(
    cli: Option<String>,
    config: &KoeConfig,
) -> String {
    coalesce_owned(
        cli,
        config
            .transcription
            .locale
            .as_deref()
            .or(config.defaults.locale.as_deref()),
        builtin::LOCALE,
    )
}

/// Transcript format for `koe transcribe`.
#[must_use]
pub fn transcribe_format(
    cli: Option<String>,
    config: &KoeConfig,
) -> String {
    coalesce_owned(
        cli,
        config
            .transcription
            .transcript_format
            .as_deref()
            .or(config.defaults.transcript_format.as_deref()),
        builtin::TRANSCRIPT_FORMAT,
    )
}

/// Speech engine for `koe transcribe`: CLI > `[transcription]` > `[defaults]` > built-in.
#[must_use]
pub fn transcribe_engine(
    cli: Option<String>,
    config: &KoeConfig,
) -> String {
    coalesce_owned(
        cli,
        config
            .transcription
            .engine
            .as_deref()
            .or(config.defaults.engine.as_deref()),
        builtin::SPEECH_ENGINE,
    )
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_spec_example() {
        let config = parse_toml(
            r#"
[defaults]
source = "system"
format = "ogg"
locale = "en-US"
transcript-format = "txt"
engine = "auto"
sample-rate = 48000
channels = 2

[aec]
enabled = true
comfort-noise = true

[output]
directory = "~/Recordings/Koe"

[transcription]
locale = "en-US"
transcript-format = "srt"
engine = "on-device"
"#,
        )
        .expect("parse");

        assert_eq!(config.defaults.source.as_deref(), Some("system"));
        assert_eq!(config.defaults.transcript_format.as_deref(), Some("txt"));
        assert_eq!(config.defaults.engine.as_deref(), Some("auto"));
        assert_eq!(config.aec.enabled, Some(true));
        assert_eq!(config.aec.comfort_noise, Some(true));
        assert_eq!(config.output.directory.as_deref(), Some("~/Recordings/Koe"));
        assert_eq!(
            config.transcription.transcript_format.as_deref(),
            Some("srt")
        );
        assert_eq!(config.transcription.engine.as_deref(), Some("on-device"));
    }

    #[test]
    fn empty_toml_is_default() {
        assert_eq!(parse_toml("").expect("parse"), KoeConfig::default());
    }

    #[test]
    fn partial_sections_ok() {
        let config = parse_toml(
            r#"
[defaults]
format = "flac"
[aec]
enabled = false
"#,
        )
        .expect("parse");
        assert_eq!(config.defaults.format.as_deref(), Some("flac"));
        assert!(config.defaults.source.is_none());
        assert_eq!(config.aec.enabled, Some(false));
        assert!(config.aec.comfort_noise.is_none());
    }

    #[test]
    fn coalesce_prefers_cli_then_config() {
        assert_eq!(
            coalesce_owned(Some("mic".into()), Some("system"), builtin::SOURCE),
            "mic"
        );
        assert_eq!(coalesce_owned(None, Some("both"), builtin::SOURCE), "both");
        assert_eq!(coalesce_owned(None, None, builtin::SOURCE), "system");
        assert_eq!(coalesce_copy(Some(44_100), Some(48_000), 48_000), 44_100);
        assert_eq!(coalesce_copy(None, Some(44_100), 48_000), 44_100);
        assert_eq!(coalesce_copy(None, None, 48_000), 48_000);
    }

    #[test]
    fn transcribe_falls_back_through_sections() {
        let mut config = KoeConfig::default();
        config.defaults.locale = Some("ja-JP".into());
        config.defaults.transcript_format = Some("txt".into());
        config.defaults.engine = Some("network".into());
        assert_eq!(transcribe_locale(None, &config), "ja-JP");
        assert_eq!(transcribe_format(None, &config), "txt");
        assert_eq!(transcribe_engine(None, &config), "network");

        config.transcription.locale = Some("fr-FR".into());
        config.transcription.transcript_format = Some("srt".into());
        config.transcription.engine = Some("on-device".into());
        assert_eq!(transcribe_locale(None, &config), "fr-FR");
        assert_eq!(transcribe_format(Some("vtt".into()), &config), "vtt");
        assert_eq!(transcribe_engine(Some("auto".into()), &config), "auto");
    }

    #[test]
    fn expand_tilde_uses_home() {
        let home = home_dir().expect("HOME");
        assert_eq!(
            expand_tilde("~/Recordings/Koe"),
            home.join("Recordings/Koe")
        );
        assert_eq!(expand_tilde("~"), home);
        assert_eq!(expand_tilde("/abs/path"), PathBuf::from("/abs/path"));
        assert_eq!(expand_tilde("relative"), PathBuf::from("relative"));
    }

    #[test]
    fn resolve_relative_output_under_directory() {
        let mut config = KoeConfig::default();
        config.output.directory = Some("/tmp/koe-out".into());
        assert_eq!(
            resolve_under_output_dir(Path::new("meeting.ogg"), &config),
            PathBuf::from("/tmp/koe-out/meeting.ogg")
        );
        assert_eq!(
            resolve_under_output_dir(Path::new("/abs/out.ogg"), &config),
            PathBuf::from("/abs/out.ogg")
        );
    }

    #[test]
    fn rejects_unknown_keys() {
        let err = parse_toml("[defaults]\nunknown-key = 1\n").expect_err("unknown");
        assert!(
            err.contains("unknown") || err.contains("unknown-key") || err.contains("did you mean")
        );
    }

    #[test]
    fn load_explicit_missing_errors() {
        let err = load(Some(Path::new("/tmp/koe-definitely-missing-97e3.toml")))
            .expect_err("explicit missing");
        assert!(err.contains("not found"));
    }

    #[test]
    fn load_explicit_file() {
        let dir = std::env::temp_dir().join(format!(
            "koe-config-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            r#"
[defaults]
source = "mic"
format = "wav"
"#,
        )
        .expect("write");

        let config = load(Some(&path)).expect("load");
        assert_eq!(config.defaults.source.as_deref(), Some("mic"));
        assert_eq!(config.defaults.format.as_deref(), Some("wav"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}

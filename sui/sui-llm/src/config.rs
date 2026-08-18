use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use reqwest::Url;
use serde::Deserialize;

use crate::LlmError;

/// OpenAI-compatible API used by [`crate::LlmClient`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum ApiMode {
    /// The `/chat/completions` endpoint.
    #[default]
    ChatCompletions,
    /// The `/responses` endpoint.
    Responses,
}

impl ApiMode {
    fn parse(value: &str) -> Result<Self, LlmError> {
        match value.trim().to_ascii_lowercase().as_str() {
            "chat_completions" | "chat-completions" | "chat" => Ok(Self::ChatCompletions),
            "responses" | "response" => Ok(Self::Responses),
            _ => Err(LlmError::InvalidConfig(
                "api_mode must be `chat_completions` or `responses`".into(),
            )),
        }
    }
}

/// Connection settings for an OpenAI-compatible API.
///
/// `base_url` is trusted operator configuration. Validation allows `http` and
/// `https`, requires a host, rejects userinfo, and normalizes the path to end
/// in `/v1`. Cleartext `http` remains useful for local endpoints.
///
/// The API key is stored as a plain [`String`]; [`Debug`](std::fmt::Debug)
/// redacts non-empty values. Prefer not to log configs in production.
#[derive(Clone, PartialEq, Eq)]
pub struct LlmConfig {
    base_url: String,
    api_key: String,
    model: String,
    api_mode: ApiMode,
}

impl std::fmt::Debug for LlmConfig {
    fn fmt(
        &self,
        f: &mut std::fmt::Formatter<'_>,
    ) -> std::fmt::Result {
        f.debug_struct("LlmConfig")
            .field("base_url", &self.base_url)
            .field("api_key", &redact_secret(&self.api_key))
            .field("model", &self.model)
            .field("api_mode", &self.api_mode)
            .finish()
    }
}

impl LlmConfig {
    /// Creates chat-completions config from explicit values.
    ///
    /// `base_url` may omit the `/v1` suffix. Use [`Self::new_with_mode`] or
    /// [`Self::with_api_mode`] to select the Responses API.
    ///
    /// # Errors
    ///
    /// Returns [`LlmError::InvalidConfig`] for invalid URL, API key, or model
    /// values.
    pub fn new(
        base_url: impl AsRef<str>,
        api_key: impl Into<String>,
        model: impl AsRef<str>,
    ) -> Result<Self, LlmError> {
        Self::new_with_mode(base_url, api_key, model, ApiMode::ChatCompletions)
    }

    /// Creates config with an explicit API mode.
    ///
    /// # Errors
    ///
    /// Returns [`LlmError::InvalidConfig`] for invalid URL, API key, or model
    /// values.
    pub fn new_with_mode(
        base_url: impl AsRef<str>,
        api_key: impl Into<String>,
        model: impl AsRef<str>,
        api_mode: ApiMode,
    ) -> Result<Self, LlmError> {
        let base_url = normalize_base_url(base_url.as_ref())?;
        let api_key = api_key.into();
        validate_api_key(&api_key)?;
        let model = require_non_empty_model(model.as_ref())?;
        Ok(Self {
            base_url,
            api_key,
            model: model.to_owned(),
            api_mode,
        })
    }

    /// Returns this config with a different API mode.
    #[must_use]
    pub const fn with_api_mode(
        mut self,
        api_mode: ApiMode,
    ) -> Self {
        self.api_mode = api_mode;
        self
    }

    /// Loads `SUI_LLM_BASE_URL`, optional `SUI_LLM_API_KEY`,
    /// `SUI_LLM_MODEL`, and optional `SUI_LLM_API_MODE`.
    ///
    /// The old `LITELLM_*` names are accepted as fallbacks for migration.
    /// Canonical `SUI_LLM_*` values always win when both names are set.
    ///
    /// # Errors
    ///
    /// Returns [`LlmError::MissingEnv`] when a required variable is unset, or
    /// [`LlmError::InvalidConfig`] when values fail validation.
    pub fn from_env() -> Result<Self, LlmError> {
        Self::from_lookup(|key| std::env::var(key))
    }

    /// Loads the `[llm]` section from the default `config.toml`.
    ///
    /// The path is resolved by [`default_config_path`]. A document containing
    /// only other sections, such as `[theme]`, is treated as missing so callers
    /// can fall back to environment configuration.
    ///
    /// # Errors
    ///
    /// Returns [`LlmError::MissingConfig`] when no file or `[llm]` section is
    /// present, [`LlmError::ConfigFile`] for other read failures, or
    /// [`LlmError::InvalidConfig`] for malformed values.
    pub fn from_config() -> Result<Self, LlmError> {
        let Some(path) = default_config_path() else {
            return Err(LlmError::MissingConfig);
        };
        Self::from_config_path(path)
    }

    /// Loads the `[llm]` section from an explicit TOML path.
    ///
    /// # Errors
    ///
    /// See [`Self::from_config`].
    pub fn from_config_path(path: impl AsRef<Path>) -> Result<Self, LlmError> {
        let raw = match std::fs::read_to_string(path.as_ref()) {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(LlmError::MissingConfig);
            },
            Err(error) => return Err(LlmError::ConfigFile(error)),
        };
        Self::from_toml(&raw)
    }

    /// Parses a TOML document containing an `[llm]` section.
    ///
    /// Expected shape:
    ///
    /// ```toml
    /// [llm]
    /// base_url = "https://api.openai.com/v1"
    /// api_key = "sk-..."
    /// model = "gpt-4o"
    /// api_mode = "chat_completions"
    /// ```
    ///
    /// This parser requires the file's `base_url` and `model` fields. The
    /// environment-aware [`Self::from_config_or_env`] fills missing fields
    /// from canonical `SUI_LLM_*` variables, then the legacy `LITELLM_*`
    /// aliases.
    ///
    /// # Errors
    ///
    /// Returns [`LlmError::MissingConfig`] without an `[llm]` section, or
    /// [`LlmError::InvalidConfig`] for malformed TOML or values.
    pub fn from_toml(input: &str) -> Result<Self, LlmError> {
        let document = toml::from_str::<FileConfig>(input)
            .map_err(|_| LlmError::InvalidConfig("invalid config.toml".into()))?;
        let Some(llm) = document.llm else {
            return Err(LlmError::MissingConfig);
        };
        let base_url = llm
            .base_url
            .ok_or_else(|| LlmError::InvalidConfig("[llm] requires `base_url`".into()))?;
        let model = llm
            .model
            .ok_or_else(|| LlmError::InvalidConfig("[llm] requires `model`".into()))?;
        let api_mode = llm
            .api_mode
            .as_deref()
            .map(ApiMode::parse)
            .transpose()?
            .unwrap_or_default();
        Self::new_with_mode(base_url, llm.api_key.unwrap_or_default(), model, api_mode)
    }

    /// Loads the default TOML config, falling back to environment settings
    /// when the file or its `[llm]` section is absent.
    ///
    /// # Errors
    ///
    /// Propagates configuration-file, environment, and validation errors.
    pub fn from_config_or_env() -> Result<Self, LlmError> {
        Self::from_config_or_env_at(default_config_path(), |key| std::env::var(key))
    }

    /// Normalized OpenAI-compatible API base (includes `/v1`).
    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// API credential. It may be empty for an open local endpoint.
    #[must_use]
    pub fn api_key(&self) -> &str {
        &self.api_key
    }

    /// Default model name for chat calls.
    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }

    /// API mode used by this config.
    #[must_use]
    pub const fn api_mode(&self) -> ApiMode {
        self.api_mode
    }

    fn from_lookup<F>(mut lookup: F) -> Result<Self, LlmError>
    where
        F: FnMut(&str) -> Result<String, std::env::VarError>,
    {
        let base_url = lookup_env(&mut lookup, "SUI_LLM_BASE_URL", "LITELLM_BASE_URL")?
            .ok_or(LlmError::MissingEnv("SUI_LLM_BASE_URL"))?;
        let api_key =
            lookup_env(&mut lookup, "SUI_LLM_API_KEY", "LITELLM_API_KEY")?.unwrap_or_default();
        let model = lookup_env(&mut lookup, "SUI_LLM_MODEL", "LITELLM_MODEL")?
            .ok_or(LlmError::MissingEnv("SUI_LLM_MODEL"))?;
        let api_mode = lookup_env(&mut lookup, "SUI_LLM_API_MODE", "LITELLM_API_MODE")?
            .map(|value| ApiMode::parse(&value))
            .transpose()?
            .unwrap_or_default();
        Self::new_with_mode(base_url, api_key, model, api_mode)
    }

    fn from_config_path_and_lookup<F>(
        path: impl AsRef<Path>,
        mut lookup: F,
    ) -> Result<Self, LlmError>
    where
        F: FnMut(&str) -> Result<String, std::env::VarError>,
    {
        let raw = match std::fs::read_to_string(path.as_ref()) {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(LlmError::MissingConfig);
            },
            Err(error) => return Err(LlmError::ConfigFile(error)),
        };
        let document = toml::from_str::<FileConfig>(&raw)
            .map_err(|_| LlmError::InvalidConfig("invalid config.toml".into()))?;
        let Some(llm) = document.llm else {
            return Err(LlmError::MissingConfig);
        };
        Self::from_file_and_lookup(llm, |key| lookup(key))
    }

    fn from_config_or_env_at<F>(
        path: Option<PathBuf>,
        mut lookup: F,
    ) -> Result<Self, LlmError>
    where
        F: FnMut(&str) -> Result<String, std::env::VarError>,
    {
        match path {
            Some(path) => match Self::from_config_path_and_lookup(&path, &mut lookup) {
                Ok(config) => Ok(config),
                Err(LlmError::MissingConfig) => Self::from_lookup(lookup),
                Err(error) => Err(error),
            },
            None => Self::from_lookup(lookup),
        }
    }

    fn from_file_and_lookup<F>(
        llm: FileLlmConfig,
        mut lookup: F,
    ) -> Result<Self, LlmError>
    where
        F: FnMut(&str) -> Result<String, std::env::VarError>,
    {
        let base_url = match llm.base_url {
            Some(value) => value,
            None => lookup_env(&mut lookup, "SUI_LLM_BASE_URL", "LITELLM_BASE_URL")?
                .ok_or(LlmError::MissingEnv("SUI_LLM_BASE_URL"))?,
        };
        let api_key = match llm.api_key {
            Some(value) => value,
            None => {
                lookup_env(&mut lookup, "SUI_LLM_API_KEY", "LITELLM_API_KEY")?.unwrap_or_default()
            },
        };
        let model = match llm.model {
            Some(value) => value,
            None => lookup_env(&mut lookup, "SUI_LLM_MODEL", "LITELLM_MODEL")?
                .ok_or(LlmError::MissingEnv("SUI_LLM_MODEL"))?,
        };
        let api_mode = match llm.api_mode {
            Some(value) => ApiMode::parse(&value)?,
            None => lookup_env(&mut lookup, "SUI_LLM_API_MODE", "LITELLM_API_MODE")?
                .map(|value| ApiMode::parse(&value))
                .transpose()?
                .unwrap_or_default(),
        };
        Self::new_with_mode(base_url, api_key, model, api_mode)
    }
}

/// A named model loaded from a `[[model.<name>]]` section of `config.toml`.
///
/// Each section supplies the same connection settings as `[llm]`. The optional
/// `env_key` field names an environment variable that provides the API key, so
/// secrets stay out of the config file:
///
/// ```toml
/// [[model."gpt4o"]]
/// base_url = "https://api.openai.com/v1"
/// model = "gpt-4o"
/// env_key = "OPENAI_API_KEY"
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlmModel {
    name: String,
    config: LlmConfig,
}

impl LlmModel {
    /// Creates a named model from an explicit name and resolved config.
    ///
    /// # Errors
    ///
    /// Returns [`LlmError::InvalidConfig`] when `name` is empty or contains
    /// whitespace. Names are typed as `/model <name>`, so keeping them as a
    /// single shell-like token avoids ambiguous command parsing.
    pub fn new(
        name: impl Into<String>,
        config: LlmConfig,
    ) -> Result<Self, LlmError> {
        let name = name.into();
        validate_model_section_name(&name)?;
        Ok(Self { name, config })
    }

    /// The section name (the `<name>` in `[[model.<name>]]`).
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The resolved connection settings for this model.
    #[must_use]
    pub const fn config(&self) -> &LlmConfig {
        &self.config
    }

    /// Loads every `[[model.<name>]]` section from the default `config.toml`.
    ///
    /// Returns an empty vector when the file, or its `[model]` table, is
    /// absent. Models are returned in lexicographic section-name order. The
    /// path is resolved by [`default_config_path`].
    ///
    /// # Errors
    ///
    /// Returns [`LlmError::ConfigFile`] for read failures other than a missing
    /// file, or [`LlmError::InvalidConfig`] for malformed values (including an
    /// `env_key` whose environment variable is unset).
    pub fn from_config() -> Result<Vec<Self>, LlmError> {
        let Some(path) = default_config_path() else {
            return Ok(Vec::new());
        };
        Self::from_config_path(path)
    }

    /// Loads `[[model.<name>]]` sections from an explicit TOML path.
    ///
    /// # Errors
    ///
    /// See [`Self::from_config`].
    pub fn from_config_path(path: impl AsRef<Path>) -> Result<Vec<Self>, LlmError> {
        let raw = match std::fs::read_to_string(path.as_ref()) {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(LlmError::ConfigFile(error)),
        };
        Self::from_toml(&raw)
    }

    /// Parses `[[model.<name>]]` sections from a TOML document, resolving each
    /// `env_key` against the process environment.
    ///
    /// # Errors
    ///
    /// See [`Self::from_config`].
    pub fn from_toml(input: &str) -> Result<Vec<Self>, LlmError> {
        Self::from_toml_with_lookup(input, |key| std::env::var(key))
    }

    fn from_toml_with_lookup<F>(
        input: &str,
        mut lookup: F,
    ) -> Result<Vec<Self>, LlmError>
    where
        F: FnMut(&str) -> Result<String, std::env::VarError>,
    {
        let document = toml::from_str::<FileConfig>(input)
            .map_err(|_| LlmError::InvalidConfig("invalid config.toml".into()))?;
        let Some(sections) = document.model else {
            return Ok(Vec::new());
        };
        let mut models = Vec::with_capacity(sections.len());
        for (name, entries) in sections {
            models.push(resolve_model(name, entries, &mut lookup)?);
        }
        Ok(models)
    }
}

fn resolve_model<F>(
    name: String,
    entries: Vec<FileModelEntry>,
    lookup: &mut F,
) -> Result<LlmModel, LlmError>
where
    F: FnMut(&str) -> Result<String, std::env::VarError>,
{
    validate_model_section_name(&name)?;
    let mut entries = entries.into_iter();
    let entry = entries
        .next()
        .ok_or_else(|| LlmError::InvalidConfig(format!("[[model.{name}]] has no entries")))?;
    if entries.next().is_some() {
        return Err(LlmError::InvalidConfig(format!(
            "[[model.{name}]] is defined more than once"
        )));
    }
    let base_url = entry
        .base_url
        .ok_or_else(|| LlmError::InvalidConfig(format!("[[model.{name}]] requires `base_url`")))?;
    let model = entry
        .model
        .ok_or_else(|| LlmError::InvalidConfig(format!("[[model.{name}]] requires `model`")))?;
    let api_key = match (entry.api_key, entry.env_key) {
        (Some(_), Some(_)) => {
            return Err(LlmError::InvalidConfig(format!(
                "[[model.{name}]] sets both `api_key` and `env_key`; use one"
            )));
        },
        (Some(api_key), None) => api_key,
        (None, Some(env_key)) => resolve_env_key(&name, &env_key, lookup)?,
        (None, None) => String::new(),
    };
    let api_mode = entry
        .api_mode
        .as_deref()
        .map(ApiMode::parse)
        .transpose()?
        .unwrap_or_default();
    let config = LlmConfig::new_with_mode(base_url, api_key, model, api_mode)?;
    LlmModel::new(name, config)
}

fn validate_model_section_name(name: &str) -> Result<(), LlmError> {
    if name.trim().is_empty() {
        return Err(LlmError::InvalidConfig(
            "model name must not be empty".into(),
        ));
    }
    if name.chars().any(char::is_whitespace) {
        return Err(LlmError::InvalidConfig(format!(
            "model name `{name}` must not contain whitespace"
        )));
    }
    Ok(())
}

fn resolve_env_key<F>(
    name: &str,
    env_key: &str,
    lookup: &mut F,
) -> Result<String, LlmError>
where
    F: FnMut(&str) -> Result<String, std::env::VarError>,
{
    validate_env_key(name, env_key)?;
    match lookup(env_key) {
        Ok(value) => Ok(value),
        Err(std::env::VarError::NotPresent) => Err(LlmError::InvalidConfig(format!(
            "[[model.{name}]] env_key `{env_key}` is not set"
        ))),
        Err(std::env::VarError::NotUnicode(_)) => Err(LlmError::InvalidConfig(format!(
            "[[model.{name}]] env_key `{env_key}` must be valid UTF-8"
        ))),
    }
}

fn validate_env_key(
    name: &str,
    env_key: &str,
) -> Result<(), LlmError> {
    if env_key.is_empty() || env_key.chars().any(|ch| ch == '=' || ch == '\0') {
        return Err(LlmError::InvalidConfig(format!(
            "[[model.{name}]] env_key must be a valid environment variable name"
        )));
    }
    Ok(())
}

#[derive(Deserialize)]
struct FileConfig {
    llm: Option<FileLlmConfig>,
    model: Option<BTreeMap<String, Vec<FileModelEntry>>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileLlmConfig {
    base_url: Option<String>,
    api_key: Option<String>,
    model: Option<String>,
    api_mode: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileModelEntry {
    base_url: Option<String>,
    api_key: Option<String>,
    env_key: Option<String>,
    model: Option<String>,
    api_mode: Option<String>,
}

fn lookup_env<F>(
    lookup: &mut F,
    canonical: &'static str,
    legacy: &'static str,
) -> Result<Option<String>, LlmError>
where
    F: FnMut(&str) -> Result<String, std::env::VarError>,
{
    match lookup(canonical) {
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotUnicode(_)) => Err(LlmError::InvalidConfig(format!(
            "{canonical} must be valid UTF-8"
        ))),
        Err(std::env::VarError::NotPresent) => match lookup(legacy) {
            Ok(value) => Ok(Some(value)),
            Err(std::env::VarError::NotPresent) => Ok(None),
            Err(std::env::VarError::NotUnicode(_)) => Err(LlmError::InvalidConfig(format!(
                "{legacy} must be valid UTF-8"
            ))),
        },
    }
}

/// Resolves the default `config.toml` path using the repository convention.
///
/// `$SUI_CONFIG` takes precedence, followed by
/// `$XDG_CONFIG_HOME/sui/config.toml`, then `~/.config/sui/config.toml`.
#[must_use]
pub fn default_config_path() -> Option<PathBuf> {
    default_config_path_from(|key| std::env::var_os(key))
}

fn default_config_path_from<F>(mut lookup: F) -> Option<PathBuf>
where
    F: FnMut(&str) -> Option<std::ffi::OsString>,
{
    if let Some(path) = lookup("SUI_CONFIG").filter(|path| !path.is_empty()) {
        return Some(PathBuf::from(path));
    }
    let base = lookup("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| lookup("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(base.join("sui").join("config.toml"))
}

/// Trims and rejects an empty logical model name.
fn require_non_empty_model(model: &str) -> Result<&str, LlmError> {
    let trimmed = model.trim();
    if trimmed.is_empty() {
        return Err(LlmError::InvalidConfig(
            "model must be a non-empty string".into(),
        ));
    }
    Ok(trimmed)
}

fn validate_api_key(api_key: &str) -> Result<(), LlmError> {
    if !api_key.bytes().all(|byte| (0x21..=0x7e).contains(&byte)) {
        return Err(LlmError::InvalidConfig(
            "api_key must be printable ASCII without spaces or controls".into(),
        ));
    }
    Ok(())
}

fn normalize_base_url(raw: &str) -> Result<String, LlmError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(LlmError::InvalidConfig(
            "base_url must be a non-empty string".into(),
        ));
    }
    if trimmed.chars().any(char::is_control) {
        return Err(LlmError::InvalidConfig(
            "base_url must not contain control characters".into(),
        ));
    }
    if trimmed.chars().any(char::is_whitespace) {
        return Err(LlmError::InvalidConfig(
            "base_url must not contain whitespace".into(),
        ));
    }
    let Some((_, authority_and_path)) = trimmed.split_once("://") else {
        return Err(LlmError::InvalidConfig(
            "base_url must be a valid http or https URL".into(),
        ));
    };
    if authority_and_path.is_empty() || authority_and_path.starts_with(['/', '?', '#']) {
        return Err(LlmError::InvalidConfig(
            "base_url must include a host".into(),
        ));
    }
    let url = Url::parse(trimmed).map_err(|_| {
        LlmError::InvalidConfig("base_url must be a valid http or https URL".into())
    })?;
    if url.scheme() != "http" && url.scheme() != "https" {
        return Err(LlmError::InvalidConfig(
            "base_url must use http or https scheme".into(),
        ));
    }
    if url.host_str().is_none_or(str::is_empty) {
        return Err(LlmError::InvalidConfig(
            "base_url must include a host".into(),
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(LlmError::InvalidConfig(
            "base_url must not include userinfo".into(),
        ));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(LlmError::InvalidConfig(
            "base_url must not include a query or fragment".into(),
        ));
    }
    let has_v1_suffix = url
        .path()
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .is_some_and(|suffix| suffix.eq_ignore_ascii_case("v1"));
    let without_slash = trimmed.trim_end_matches('/');
    if has_v1_suffix {
        Ok(without_slash.to_owned())
    } else {
        Ok(format!("{without_slash}/v1"))
    }
}

fn redact_secret(secret: &str) -> String {
    if secret.is_empty() {
        String::new()
    } else {
        "***".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    #[test]
    fn normalizes_base_url_and_mode() {
        let cfg = LlmConfig::new("http://localhost:4000/", "k", "m").expect("config");
        assert_eq!(cfg.base_url(), "http://localhost:4000/v1");
        assert_eq!(cfg.api_key(), "k");
        assert_eq!(cfg.model(), "m");
        assert_eq!(cfg.api_mode(), ApiMode::ChatCompletions);

        let responses = LlmConfig::new_with_mode(
            "https://api.example.test/openai",
            "",
            "gpt",
            ApiMode::Responses,
        )
        .expect("responses config");
        assert_eq!(responses.base_url(), "https://api.example.test/openai/v1");
        assert_eq!(responses.api_mode(), ApiMode::Responses);

        let encoded = LlmConfig::new("http://localhost/api%20v1", "k", "m").expect("config");
        assert_eq!(encoded.base_url(), "http://localhost/api%20v1/v1");
    }

    #[test]
    fn rejects_unsafe_values() {
        for url in [
            "ftp://localhost:4000",
            "http://user:pass@localhost:4000",
            "http://localhost:4000?secret=bad",
            "http:///v1",
            "  ",
        ] {
            assert!(
                matches!(
                    LlmConfig::new(url, "k", "m"),
                    Err(LlmError::InvalidConfig(_))
                ),
                "accepted unsafe URL {url:?}"
            );
        }
        for key in ["bad key", "bad\nkey", "bad\rkey", "bad\x01key"] {
            assert!(matches!(
                LlmConfig::new("http://localhost:4000", key, "m"),
                Err(LlmError::InvalidConfig(_))
            ));
        }
        assert!(matches!(
            LlmConfig::new("http://localhost:4000", "k", "  "),
            Err(LlmError::InvalidConfig(_))
        ));
    }

    #[test]
    fn parses_llm_config_from_toml() {
        let config = LlmConfig::from_toml(
            r#"
            [theme]
            active = "dark"

            [llm]
            base_url = "https://api.example.test/v1"
            api_key = "super-secret"
            model = "gpt-5"
            api_mode = "responses"
            "#,
        )
        .expect("config");
        assert_eq!(config.base_url(), "https://api.example.test/v1");
        assert_eq!(config.api_key(), "super-secret");
        assert_eq!(config.model(), "gpt-5");
        assert_eq!(config.api_mode(), ApiMode::Responses);
    }

    #[test]
    fn parses_named_models_from_toml_and_env_key() {
        let models = LlmModel::from_toml_with_lookup(
            r#"
            [[model."gemma4"]]
            base_url = "http://localhost:11434"
            model = "gemma4:latest"

            [[model."gpt4o"]]
            base_url = "https://api.openai.com/v1"
            env_key = "OPENAI_API_KEY"
            model = "gpt-4o"
            api_mode = "responses"
            "#,
            |key| match key {
                "OPENAI_API_KEY" => Ok("env-secret".into()),
                _ => Err(std::env::VarError::NotPresent),
            },
        )
        .expect("models");
        assert_eq!(models.len(), 2);
        assert_eq!(
            models.iter().map(LlmModel::name).collect::<Vec<_>>(),
            vec!["gemma4", "gpt4o"]
        );

        let gemma = models
            .iter()
            .find(|model| model.name() == "gemma4")
            .expect("gemma4 model");
        assert_eq!(gemma.config().base_url(), "http://localhost:11434/v1");
        assert_eq!(gemma.config().api_key(), "");
        assert_eq!(gemma.config().model(), "gemma4:latest");
        assert_eq!(gemma.config().api_mode(), ApiMode::ChatCompletions);

        let gpt = models
            .iter()
            .find(|model| model.name() == "gpt4o")
            .expect("gpt4o model");
        assert_eq!(gpt.config().base_url(), "https://api.openai.com/v1");
        assert_eq!(gpt.config().api_key(), "env-secret");
        assert_eq!(gpt.config().model(), "gpt-4o");
        assert_eq!(gpt.config().api_mode(), ApiMode::Responses);
    }

    #[test]
    fn creates_named_model_from_config() {
        let config = LlmConfig::new("http://localhost:4000", "", "m").expect("config");
        let model = LlmModel::new("local", config.clone()).expect("model");
        assert_eq!(model.name(), "local");
        assert_eq!(model.config(), &config);
        assert!(matches!(
            LlmModel::new("bad name", config),
            Err(LlmError::InvalidConfig(_))
        ));
    }

    #[test]
    fn rejects_invalid_named_models() {
        assert!(matches!(
            LlmModel::from_toml_with_lookup(
                "[[model.gemma4]]\nbase_url = \"http://localhost\"\nmodel = \"m\"\napi_key = \"k\"\nenv_key = \"K\"\n",
                |_| Err(std::env::VarError::NotPresent),
            ),
            Err(LlmError::InvalidConfig(_))
        ));
        assert!(matches!(
            LlmModel::from_toml_with_lookup(
                "[[model.gemma4]]\nbase_url = \"http://localhost\"\nmodel = \"m\"\n[[model.gemma4]]\nbase_url = \"http://localhost\"\nmodel = \"m2\"\n",
                |_| Err(std::env::VarError::NotPresent),
            ),
            Err(LlmError::InvalidConfig(_))
        ));
        assert!(matches!(
            LlmModel::from_toml_with_lookup(
                "[[model.gemma4]]\nbase_url = \"http://localhost\"\nmodel = \"m\"\nenv_key = \"MISSING\"\n",
                |_| Err(std::env::VarError::NotPresent),
            ),
            Err(LlmError::InvalidConfig(_))
        ));
        assert!(matches!(
            LlmModel::from_toml_with_lookup(
                "[[model.gemma4]]\nbase_url = \"http://localhost\"\nmodel = \"m\"\nenv_key = \"BAD=KEY\"\n",
                |_| Ok("must-not-read".into()),
            ),
            Err(LlmError::InvalidConfig(_))
        ));
    }

    #[test]
    fn config_without_llm_can_fall_back_to_environment() {
        assert!(matches!(
            LlmConfig::from_toml("[theme]\nactive = \"default\"\n"),
            Err(LlmError::MissingConfig)
        ));
        assert!(matches!(
            LlmConfig::from_toml("[llm]\nbase_url = \"http://localhost\"\n"),
            Err(LlmError::InvalidConfig(_))
        ));
        assert!(matches!(
            LlmConfig::from_toml("not valid [toml"),
            Err(LlmError::InvalidConfig(_))
        ));
        assert!(matches!(
            ApiMode::parse("unknown"),
            Err(LlmError::InvalidConfig(_))
        ));
    }

    #[test]
    fn loads_config_from_explicit_path() {
        let path =
            std::env::temp_dir().join(format!("sui-llm-config-{}-test.toml", std::process::id()));
        std::fs::write(
            &path,
            "[llm]\nbase_url = \"http://localhost:4000\"\nmodel = \"local\"\n",
        )
        .expect("write config");
        let config = LlmConfig::from_config_path(&path).expect("config");
        let _ = std::fs::remove_file(&path);
        assert_eq!(config.base_url(), "http://localhost:4000/v1");
        assert_eq!(config.model(), "local");
    }

    #[test]
    fn missing_config_path_is_distinguishable_from_read_failure() {
        let path =
            std::env::temp_dir().join(format!("sui-llm-missing-{}-test.toml", std::process::id()));
        assert!(matches!(
            LlmConfig::from_config_path(path),
            Err(LlmError::MissingConfig)
        ));
    }

    #[test]
    fn environment_config_uses_generic_names() {
        let config = LlmConfig::from_lookup(|key| match key {
            "SUI_LLM_BASE_URL" => Ok("http://localhost:4000".into()),
            "SUI_LLM_MODEL" => Ok("model".into()),
            "SUI_LLM_API_MODE" => Ok("responses".into()),
            _ => Err(std::env::VarError::NotPresent),
        })
        .expect("config");
        assert_eq!(config.api_key(), "");
        assert_eq!(config.api_mode(), ApiMode::Responses);
    }

    #[test]
    fn default_path_honors_overrides_in_order() {
        let explicit = default_config_path_from(|key| match key {
            "SUI_CONFIG" => Some(OsString::from("/tmp/custom.toml")),
            _ => None,
        });
        assert_eq!(explicit, Some(PathBuf::from("/tmp/custom.toml")));

        let xdg = default_config_path_from(|key| match key {
            "XDG_CONFIG_HOME" => Some(OsString::from("/tmp/xdg")),
            "HOME" => Some(OsString::from("/tmp/home")),
            _ => None,
        });
        assert_eq!(xdg, Some(PathBuf::from("/tmp/xdg/sui/config.toml")));

        let home = default_config_path_from(|key| match key {
            "HOME" => Some(OsString::from("/tmp/home")),
            _ => None,
        });
        assert_eq!(
            home,
            Some(PathBuf::from("/tmp/home/.config/sui/config.toml"))
        );
    }

    #[test]
    fn debug_redacts_api_key() {
        let cfg = LlmConfig::new("http://localhost:4000", "super-secret", "m").expect("config");
        let rendered = format!("{cfg:?}");
        assert!(rendered.contains("***"), "{rendered}");
        assert!(!rendered.contains("super-secret"), "{rendered}");
    }

    #[test]
    fn config_file_debug_redacts_underlying_io_error() {
        let path = std::env::temp_dir().join(format!("sui-llm-redact-{}-dir", std::process::id()));
        std::fs::create_dir(&path).expect("create directory");
        let error = LlmConfig::from_config_path(&path).expect_err("unreadable path");
        let rendered = format!("{error:?}");
        assert!(rendered.contains("ConfigFile"), "{rendered}");
        assert!(!rendered.contains("temp_dir"), "{rendered}");
        assert_eq!(error.to_string(), "could not read LLM configuration file");
        let _ = std::fs::remove_dir(&path);
    }

    #[test]
    fn unknown_llm_key_is_rejected() {
        let error = LlmConfig::from_toml(
            "[llm]\nbase_url = \"http://localhost\"\nmodel = \"m\"\napi_mod = \"responses\"\n",
        )
        .expect_err("typo must be rejected");
        assert!(matches!(error, LlmError::InvalidConfig(_)));
    }

    #[test]
    fn file_values_win_but_missing_fields_fall_back_to_env() {
        let path = std::env::temp_dir().join(format!(
            "sui-llm-precedence-{}-test.toml",
            std::process::id()
        ));
        std::fs::write(
            &path,
            "[llm]\nbase_url = \"http://file.example\"\nmodel = \"file-model\"\n",
        )
        .expect("write config");
        let config = LlmConfig::from_config_path_and_lookup(&path, |key| match key {
            "SUI_LLM_API_KEY" => Ok("env-key".into()),
            "SUI_LLM_API_MODE" => Ok("responses".into()),
            _ => Err(std::env::VarError::NotPresent),
        })
        .expect("config");
        let _ = std::fs::remove_file(&path);
        assert_eq!(config.base_url(), "http://file.example/v1");
        assert_eq!(config.model(), "file-model");
        assert_eq!(config.api_key(), "env-key");
        assert_eq!(config.api_mode(), ApiMode::Responses);
    }

    #[test]
    fn config_or_env_falls_back_when_file_or_section_missing() {
        let missing =
            std::env::temp_dir().join(format!("sui-llm-or-env-{}-none.toml", std::process::id()));
        let config = LlmConfig::from_config_or_env_at(Some(missing), |key| match key {
            "SUI_LLM_BASE_URL" => Ok("http://env.example".into()),
            "SUI_LLM_MODEL" => Ok("env-model".into()),
            _ => Err(std::env::VarError::NotPresent),
        })
        .expect("fallback to env");
        assert_eq!(config.base_url(), "http://env.example/v1");
        assert_eq!(config.model(), "env-model");

        let with_section = std::env::temp_dir().join(format!(
            "sui-llm-or-env-{}-section.toml",
            std::process::id()
        ));
        std::fs::write(&with_section, "[theme]\nactive = \"dark\"\n").expect("write config");
        let config =
            LlmConfig::from_config_or_env_at(Some(with_section.clone()), |key| match key {
                "SUI_LLM_BASE_URL" => Ok("http://env.example".into()),
                "SUI_LLM_MODEL" => Ok("env-model".into()),
                _ => Err(std::env::VarError::NotPresent),
            })
            .expect("fallback to env");
        let _ = std::fs::remove_file(&with_section);
        assert_eq!(config.model(), "env-model");
    }

    #[test]
    fn config_or_env_does_not_fall_back_on_malformed_file() {
        let path =
            std::env::temp_dir().join(format!("sui-llm-or-env-{}-bad.toml", std::process::id()));
        std::fs::write(&path, "not [valid toml").expect("write config");
        let error = LlmConfig::from_config_or_env_at(Some(path.clone()), |_| {
            Ok("http://env.example".into())
        })
        .expect_err("malformed file must not fall back");
        let _ = std::fs::remove_file(&path);
        assert!(matches!(error, LlmError::InvalidConfig(_)));
    }

    #[test]
    fn config_or_env_does_not_fall_back_on_unreadable_path() {
        let path = std::env::temp_dir().join(format!("sui-llm-or-env-{}-dir", std::process::id()));
        std::fs::create_dir(&path).expect("create directory");
        let error = LlmConfig::from_config_or_env_at(Some(path.clone()), |_| {
            Ok("http://env.example".into())
        })
        .expect_err("unreadable path must not fall back");
        let _ = std::fs::remove_dir(&path);
        assert!(matches!(error, LlmError::ConfigFile(_)));
    }

    #[test]
    fn from_lookup_prefers_canonical_over_legacy() {
        let config = LlmConfig::from_lookup(|key| match key {
            "SUI_LLM_BASE_URL" => Ok("http://canonical.example".into()),
            "LITELLM_BASE_URL" => Ok("http://legacy.example".into()),
            "SUI_LLM_MODEL" => Ok("canonical-model".into()),
            "LITELLM_MODEL" => Ok("legacy-model".into()),
            "SUI_LLM_API_KEY" => Ok("env-key".into()),
            _ => Err(std::env::VarError::NotPresent),
        })
        .expect("config");
        assert_eq!(config.base_url(), "http://canonical.example/v1");
        assert_eq!(config.model(), "canonical-model");

        let legacy = LlmConfig::from_lookup(|key| match key {
            "LITELLM_BASE_URL" => Ok("http://legacy.example".into()),
            "LITELLM_MODEL" => Ok("legacy-model".into()),
            _ => Err(std::env::VarError::NotPresent),
        })
        .expect("legacy fallback");
        assert_eq!(legacy.base_url(), "http://legacy.example/v1");
        assert_eq!(legacy.model(), "legacy-model");
    }

    #[test]
    fn from_lookup_errors_on_missing_required_and_bad_values() {
        assert!(matches!(
            LlmConfig::from_lookup(|_| Err(std::env::VarError::NotPresent)),
            Err(LlmError::MissingEnv(_))
        ));
        assert!(matches!(
            LlmConfig::from_lookup(|key| match key {
                "SUI_LLM_BASE_URL" => Ok("http://localhost".into()),
                "SUI_LLM_MODEL" => Ok("m".into()),
                "SUI_LLM_API_MODE" => Ok("bogus".into()),
                _ => Err(std::env::VarError::NotPresent),
            }),
            Err(LlmError::InvalidConfig(_))
        ));
        assert!(matches!(
            LlmConfig::from_lookup(|key| match key {
                "SUI_LLM_BASE_URL" => Err(std::env::VarError::NotUnicode("bad".into())),
                _ => Err(std::env::VarError::NotPresent),
            }),
            Err(LlmError::InvalidConfig(_))
        ));
    }
}

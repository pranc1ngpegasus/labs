use std::{
    collections::BTreeMap,
    fmt::Write as _,
    fs,
    io::Read,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use yaml_serde::Value as YamlValue;

use crate::{
    error::{MemoryError, Result},
    fsutil::{create_private_dir, publish_new, write_atomic_replace},
    index,
    model::{MAX_NOTE_BYTES, Note, NoteState, NoteType, Source},
    vault::Vault,
};

pub const EVENT_SCHEMA: &str = "ren-memory-event/v1";
const MAX_EVENT_BYTES: usize = 2 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureEvent {
    pub schema: String,
    pub agent: String,
    pub event_kind: String,
    pub session_id: Option<String>,
    pub turn_id: Option<String>,
    pub vault_hint: String,
    pub occurred_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub content: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct CaptureResult {
    pub captured: bool,
    pub indexed: bool,
    pub event_key: String,
    pub note_id: String,
    pub path: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CaptureReceipt {
    schema: String,
    event_key: String,
    note_id: String,
}

pub fn parse_event(
    input: &[u8],
    expected_agent: &str,
    expected_event: &str,
) -> Result<CaptureEvent> {
    if input.len() > MAX_EVENT_BYTES {
        return Err(MemoryError::InputTooLarge {
            limit: MAX_EVENT_BYTES,
        });
    }
    let value: serde_json::Value = serde_json::from_slice(input)?;
    let event = if value.get("schema").is_some() {
        serde_json::from_value(value)?
    } else if expected_agent == "codex" {
        normalize_codex_event(&value, expected_event)?
    } else {
        return Err(MemoryError::Validation(
            "adapter payload is not a normalized ren-memory event".into(),
        ));
    };
    if event.schema != EVENT_SCHEMA {
        return Err(MemoryError::Validation(format!(
            "unsupported event schema `{}`",
            event.schema
        )));
    }
    if event.agent != expected_agent {
        return Err(MemoryError::Validation(format!(
            "event agent `{}` does not match --agent `{expected_agent}`",
            event.agent
        )));
    }
    if event.event_kind != expected_event {
        return Err(MemoryError::Validation(format!(
            "event kind `{}` does not match --event `{expected_event}`",
            event.event_kind
        )));
    }
    validate_event(&event)?;
    Ok(event)
}

pub fn read_event_stdin(
    expected_agent: &str,
    expected_event: &str,
) -> Result<CaptureEvent> {
    let mut input = Vec::new();
    std::io::stdin()
        .take(u64::try_from(MAX_EVENT_BYTES + 1).unwrap_or(u64::MAX))
        .read_to_end(&mut input)
        .map_err(|error| MemoryError::io("<stdin>", error))?;
    parse_event(&input, expected_agent, expected_event)
}

pub fn capture_event(
    vault: &Vault,
    event: &CaptureEvent,
) -> Result<CaptureResult> {
    validate_event(event)?;
    let event_key = event_key(event)?;
    let (receipt, _) = claim_capture(vault, &event_key)?;
    let content = redact_content(&event.content)?;
    let title = event
        .title
        .as_ref()
        .map(|title| redact_content(title))
        .transpose()?
        .map(|title| title.trim().to_owned())
        .or_else(|| first_nonempty_line(&content));
    let mut note = Note::new(
        NoteType::Fleeting,
        NoteState::Inbox,
        event.occurred_at.clone(),
        title,
        content,
    );
    note.frontmatter.id.clone_from(&receipt.note_id);
    note.frontmatter.sources.push(Source {
        kind: format!("{}-turn", event.agent),
        fields: source_fields(event),
    });
    insert_extra(&mut note.frontmatter.extra, "capture_event_key", &event_key);
    insert_extra(&mut note.frontmatter.extra, "capture_agent", &event.agent);
    insert_extra(
        &mut note.frontmatter.extra,
        "capture_event_kind",
        &event.event_kind,
    );
    if let Some(session_id) = &event.session_id {
        insert_extra(
            &mut note.frontmatter.extra,
            "capture_session_id",
            session_id,
        );
    }
    if let Some(turn_id) = &event.turn_id {
        insert_extra(&mut note.frontmatter.extra, "capture_turn_id", turn_id);
    }
    note.frontmatter.project = vault
        .project_path
        .file_name()
        .and_then(|value| value.to_str())
        .map(str::to_owned);
    note.frontmatter.validate()?;
    let path = vault.safe_note_path("fleeting", &note.frontmatter.id)?;
    let markdown = note.to_markdown()?;
    let existing_path = find_captured_note(vault, &receipt.note_id)?;
    let (captured, result_path) = if let Some(existing_path) = existing_path {
        let existing = crate::model::read_note(&existing_path)?;
        if yaml_string(&existing.frontmatter.extra, "capture_event_key").as_deref()
            != Some(event_key.as_str())
        {
            return Err(MemoryError::Validation(format!(
                "capture receipt for `{event_key}` points to an unrelated note"
            )));
        }
        (false, existing_path)
    } else {
        match publish_new(&path, markdown.as_bytes()) {
            Ok(()) => (true, path.clone()),
            Err(MemoryError::Io { source, .. })
                if source.kind() == std::io::ErrorKind::AlreadyExists =>
            {
                let existing = crate::model::read_note(&path)?;
                if yaml_string(&existing.frontmatter.extra, "capture_event_key").as_deref()
                    != Some(event_key.as_str())
                {
                    return Err(MemoryError::Validation(format!(
                        "capture destination for `{event_key}` was claimed by unrelated content"
                    )));
                }
                (false, path.clone())
            },
            Err(error) => return Err(error),
        }
    };
    let indexed = match index::sync(vault, false, false) {
        Ok(_) => index::capture_for_event(vault, &event_key).is_ok_and(|result| result.is_some()),
        Err(error) => {
            record_projection_diagnostic(vault, &event_key, &error);
            false
        },
    };
    if indexed {
        finish_capture_receipt(vault, &receipt)?;
        clear_projection_diagnostic(vault, &event_key);
    }
    Ok(CaptureResult {
        captured,
        indexed,
        event_key,
        note_id: note.frontmatter.id,
        path: result_path,
    })
}

fn claim_capture(
    vault: &Vault,
    event_key: &str,
) -> Result<(CaptureReceipt, bool)> {
    if let Some(receipt) = read_receipt(&completed_receipt_path(vault, event_key), event_key)? {
        return Ok((receipt, false));
    }
    let pending = pending_receipt_path(vault, event_key);
    if let Some(receipt) = read_receipt(&pending, event_key)? {
        return Ok((receipt, false));
    }
    if let Ok(Some(note_id)) = index::capture_for_event(vault, event_key) {
        let receipt = CaptureReceipt {
            schema: "ren-memory-capture-receipt/v1".into(),
            event_key: event_key.into(),
            note_id,
        };
        finish_capture_receipt(vault, &receipt)?;
        return Ok((receipt, false));
    }
    let receipt = CaptureReceipt {
        schema: "ren-memory-capture-receipt/v1".into(),
        event_key: event_key.into(),
        note_id: ulid::Ulid::generate().to_string(),
    };
    let bytes = serde_json::to_vec(&receipt)?;
    match publish_new(&pending, &bytes) {
        Ok(()) => Ok((receipt, true)),
        Err(MemoryError::Io { source, .. })
            if source.kind() == std::io::ErrorKind::AlreadyExists =>
        {
            read_receipt(&pending, event_key)?
                .map(|receipt| (receipt, false))
                .ok_or_else(|| {
                    MemoryError::Validation(format!(
                        "capture receipt `{}` disappeared during publication",
                        pending.display()
                    ))
                })
        },
        Err(error) => Err(error),
    }
}

fn pending_receipt_path(
    vault: &Vault,
    event_key: &str,
) -> PathBuf {
    vault
        .index_dir()
        .join("capture-spool")
        .join(format!("{event_key}.json"))
}

fn completed_receipt_path(
    vault: &Vault,
    event_key: &str,
) -> PathBuf {
    vault
        .index_dir()
        .join("capture-events")
        .join(&event_key[..2])
        .join(format!("{event_key}.json"))
}

fn read_receipt(
    path: &Path,
    expected_event_key: &str,
) -> Result<Option<CaptureReceipt>> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(MemoryError::io(path, error)),
    };
    let receipt: CaptureReceipt = serde_json::from_slice(&bytes)?;
    if receipt.schema != "ren-memory-capture-receipt/v1" || receipt.event_key != expected_event_key
    {
        return Err(MemoryError::Validation(format!(
            "invalid capture receipt at {}",
            path.display()
        )));
    }
    crate::model::validate_id(&receipt.note_id)?;
    Ok(Some(receipt))
}

fn finish_capture_receipt(
    vault: &Vault,
    receipt: &CaptureReceipt,
) -> Result<()> {
    let completed = completed_receipt_path(vault, &receipt.event_key);
    create_private_dir(
        completed
            .parent()
            .ok_or_else(|| MemoryError::Validation("capture receipt has no parent".into()))?,
    )?;
    let bytes = serde_json::to_vec(receipt)?;
    match publish_new(&completed, &bytes) {
        Ok(()) => {},
        Err(MemoryError::Io { source, .. })
            if source.kind() == std::io::ErrorKind::AlreadyExists =>
        {
            let existing = read_receipt(&completed, &receipt.event_key)?.ok_or_else(|| {
                MemoryError::Validation("completed capture receipt disappeared".into())
            })?;
            if existing.note_id != receipt.note_id {
                return Err(MemoryError::Validation(format!(
                    "capture event `{}` maps to conflicting note ids",
                    receipt.event_key
                )));
            }
        },
        Err(error) => return Err(error),
    }
    let pending = pending_receipt_path(vault, &receipt.event_key);
    match fs::remove_file(&pending) {
        Ok(()) => {},
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
        Err(error) => return Err(MemoryError::io(&pending, error)),
    }
    Ok(())
}

fn find_captured_note(
    vault: &Vault,
    note_id: &str,
) -> Result<Option<PathBuf>> {
    for directory in [
        "fleeting",
        "literature",
        "permanent",
        "structure",
        "index",
        "archived",
    ] {
        let path = vault.safe_note_path(directory, note_id)?;
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(MemoryError::UnsafeInput(format!(
                    "captured note path is a symlink: {}",
                    path.display()
                )));
            },
            Ok(_) => return Ok(Some(path)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
            Err(error) => return Err(MemoryError::io(&path, error)),
        }
    }
    Ok(None)
}

fn projection_diagnostic_path(
    vault: &Vault,
    event_key: &str,
) -> PathBuf {
    vault
        .index_dir()
        .join("diagnostics")
        .join(format!("capture-{event_key}.json"))
}

fn record_projection_diagnostic(
    vault: &Vault,
    event_key: &str,
    error: &MemoryError,
) {
    let value = serde_json::json!({
        "schema": "ren-memory-capture-diagnostic/v1",
        "event_key": event_key,
        "class": error.class(),
        "message": error.to_string(),
    });
    if let Ok(bytes) = serde_json::to_vec_pretty(&value) {
        let _ = write_atomic_replace(&projection_diagnostic_path(vault, event_key), &bytes);
    }
}

fn clear_projection_diagnostic(
    vault: &Vault,
    event_key: &str,
) {
    let path = projection_diagnostic_path(vault, event_key);
    if let Err(error) = fs::remove_file(&path)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        // Diagnostics are advisory and must not turn a durable capture into a
        // hook failure.
    }
}

pub fn reconcile_receipts(vault: &Vault) -> Result<()> {
    let pending_root = vault.index_dir().join("capture-spool");
    let entries = match fs::read_dir(&pending_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(MemoryError::io(&pending_root, error)),
    };
    for entry in entries {
        let entry = entry.map_err(|error| MemoryError::io(&pending_root, error))?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let event_key = path
            .file_stem()
            .and_then(|value| value.to_str())
            .ok_or_else(|| MemoryError::Validation("capture receipt name is not UTF-8".into()))?;
        let receipt = read_receipt(&path, event_key)?.ok_or_else(|| {
            MemoryError::Validation(format!("capture receipt disappeared: {}", path.display()))
        })?;
        if index::capture_for_event(vault, event_key)?.as_deref() == Some(&receipt.note_id) {
            finish_capture_receipt(vault, &receipt)?;
            clear_projection_diagnostic(vault, event_key);
        }
    }
    Ok(())
}

pub fn manual_event(
    vault: &Vault,
    content: &str,
    title: Option<&str>,
    occurred_at: String,
) -> CaptureEvent {
    CaptureEvent {
        schema: EVENT_SCHEMA.into(),
        agent: "manual".into(),
        event_kind: "capture".into(),
        session_id: None,
        turn_id: None,
        vault_hint: vault.project_path.to_string_lossy().into_owned(),
        occurred_at,
        title: title.map(str::to_owned),
        content: content.to_owned(),
    }
}

fn validate_event(event: &CaptureEvent) -> Result<()> {
    if event.schema != EVENT_SCHEMA {
        return Err(MemoryError::Validation(format!(
            "unsupported event schema `{}`",
            event.schema
        )));
    }
    for (name, value) in [
        ("agent", event.agent.as_str()),
        ("event_kind", event.event_kind.as_str()),
        ("vault_hint", event.vault_hint.as_str()),
        ("occurred_at", event.occurred_at.as_str()),
    ] {
        if value.trim().is_empty() {
            return Err(MemoryError::Validation(format!("{name} must not be empty")));
        }
    }
    for (name, value) in [
        ("session_id", event.session_id.as_deref()),
        ("turn_id", event.turn_id.as_deref()),
    ] {
        if let Some(value) = value {
            if value.len() > 1024 {
                return Err(MemoryError::InputTooLarge { limit: 1024 });
            }
            if redact_content(value)? != value {
                return Err(MemoryError::UnsafeInput(format!(
                    "{name} contains credential-like material"
                )));
            }
        }
    }
    if event.content.len() > MAX_NOTE_BYTES {
        return Err(MemoryError::InputTooLarge {
            limit: MAX_NOTE_BYTES,
        });
    }
    if event.content.trim().is_empty()
        && event
            .title
            .as_ref()
            .is_none_or(|title| title.trim().is_empty())
    {
        return Err(MemoryError::Validation(
            "captured content must not be empty".into(),
        ));
    }
    if event
        .title
        .as_ref()
        .is_some_and(|title| title.trim().is_empty())
    {
        return Err(MemoryError::Validation(
            "capture title must not be empty".into(),
        ));
    }
    event
        .occurred_at
        .parse::<jiff::Timestamp>()
        .map_err(|error| {
            MemoryError::Validation(format!("occurred_at must be an RFC3339 timestamp: {error}"))
        })?;
    Ok(())
}

fn normalize_codex_event(
    value: &serde_json::Value,
    expected_event: &str,
) -> Result<CaptureEvent> {
    let object = value.as_object().ok_or_else(|| {
        MemoryError::Validation("Codex hook payload must be a JSON object".into())
    })?;
    let hook_event = json_string(object, "hook_event_name")?;
    if !hook_event.eq_ignore_ascii_case(expected_event) {
        return Err(MemoryError::Validation(format!(
            "Codex hook event `{hook_event}` does not match --event `{expected_event}`"
        )));
    }
    Ok(CaptureEvent {
        schema: EVENT_SCHEMA.into(),
        agent: "codex".into(),
        event_kind: expected_event.to_ascii_lowercase(),
        session_id: optional_json_string(object, "session_id")?,
        turn_id: optional_json_string(object, "turn_id")?,
        vault_hint: json_string(object, "cwd")?,
        occurred_at: jiff::Timestamp::now().to_string(),
        title: None,
        content: json_string(object, "last_assistant_message")?,
    })
}

fn json_string(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<String> {
    optional_json_string(object, key)?
        .ok_or_else(|| MemoryError::Validation(format!("Codex payload is missing `{key}`")))
}

fn optional_json_string(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<Option<String>> {
    match object.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(MemoryError::Validation(format!(
            "Codex payload field `{key}` must be a string"
        ))),
    }
}

fn event_key(event: &CaptureEvent) -> Result<String> {
    let canonical = serde_json::to_vec(&serde_json::json!({
        "schema": event.schema,
        "agent": event.agent,
        "event_kind": event.event_kind,
        "session_id": event.session_id,
        "turn_id": event.turn_id,
        "vault_hint": event.vault_hint,
        "title": event.title,
        "content": event.content,
    }))?;
    Ok(hex_digest(&canonical))
}

fn redact_content(input: &str) -> Result<String> {
    if input.lines().any(|line| {
        line.contains("-----BEGIN ")
            && (line.contains("PRIVATE KEY-----") || line.contains("PGP SECRET KEY BLOCK-----"))
    }) {
        return Err(MemoryError::UnsafeInput(
            "private-key material was detected".into(),
        ));
    }
    let mut lines = Vec::new();
    for line in input.lines() {
        lines.push(redact_line(line));
    }
    let mut output = lines.join("\n");
    if input.ends_with('\n') {
        output.push('\n');
    }
    Ok(output)
}

fn redact_line(line: &str) -> String {
    let lower = line.to_ascii_lowercase();
    if contains_aws_access_key(line)
        || [
            ("ghp_", 16),
            ("github_pat_", 16),
            ("glpat-", 16),
            ("xoxb-", 16),
            ("xoxp-", 16),
            ("npm_", 16),
            ("sk-", 16),
            ("aiza", 24),
        ]
        .iter()
        .any(|(prefix, minimum)| contains_prefixed_token(&lower, prefix, *minimum))
        || contains_jwt(&lower)
        || lower.contains("authorization: bearer ")
        || lower.contains("authorization = \"bearer ")
    {
        return "[REDACTED CREDENTIAL]".into();
    }

    for key in [
        "password",
        "passwd",
        "api_key",
        "api-key",
        "apikey",
        "secret_key",
        "client_secret",
        "access_token",
        "auth_token",
        "token",
    ] {
        let mut offset = 0;
        while let Some(relative) = lower[offset..].find(key) {
            let position = offset + relative;
            let before_ok =
                position == 0 || !lower.as_bytes()[position - 1].is_ascii_alphanumeric();
            let mut cursor = position + key.len();
            while lower
                .as_bytes()
                .get(cursor)
                .is_some_and(|byte| byte.is_ascii_whitespace() || matches!(byte, b'"' | b'\''))
            {
                cursor += 1;
            }
            if before_ok
                && lower
                    .as_bytes()
                    .get(cursor)
                    .is_some_and(|byte| matches!(byte, b':' | b'='))
            {
                cursor += 1;
                while lower
                    .as_bytes()
                    .get(cursor)
                    .is_some_and(u8::is_ascii_whitespace)
                {
                    cursor += 1;
                }
                return format!("{}[REDACTED]", &line[..cursor]);
            }
            offset = position + key.len();
        }
    }
    line.to_owned()
}

fn contains_prefixed_token(
    line: &str,
    prefix: &str,
    minimum_suffix: usize,
) -> bool {
    let mut offset = 0;
    while let Some(relative) = line[offset..].find(prefix) {
        let start = offset + relative + prefix.len();
        let suffix = line[start..]
            .chars()
            .take_while(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '_' | '-')
            })
            .count();
        if suffix >= minimum_suffix {
            return true;
        }
        offset = start;
    }
    false
}

fn contains_jwt(line: &str) -> bool {
    line.split_whitespace().any(|word| {
        word.len() >= 32
            && word.starts_with("eyj")
            && word.bytes().filter(|byte| *byte == b'.').count() == 2
    })
}

fn contains_aws_access_key(line: &str) -> bool {
    line.as_bytes().windows(20).any(|window| {
        window.starts_with(b"AKIA")
            && window
                .iter()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
    })
}

fn source_fields(event: &CaptureEvent) -> BTreeMap<String, YamlValue> {
    let mut fields = BTreeMap::new();
    if let Some(session_id) = &event.session_id {
        insert_extra(&mut fields, "session_id", session_id);
    }
    if let Some(turn_id) = &event.turn_id {
        insert_extra(&mut fields, "turn_id", turn_id);
    }
    fields
}

fn insert_extra(
    fields: &mut BTreeMap<String, YamlValue>,
    key: &str,
    value: &str,
) {
    fields.insert(key.into(), YamlValue::String(value.into()));
}

fn yaml_string(
    fields: &BTreeMap<String, YamlValue>,
    key: &str,
) -> Option<String> {
    match fields.get(key) {
        Some(YamlValue::String(value)) => Some(value.clone()),
        _ => None,
    }
}

fn first_nonempty_line(content: &str) -> Option<String> {
    content.lines().find_map(|line| {
        let line = line.trim();
        if line.is_empty() {
            None
        } else {
            Some(line.chars().take(120).collect())
        }
    })
}

fn hex_digest(input: &[u8]) -> String {
    let mut output = String::with_capacity(64);
    for byte in Sha256::digest(input) {
        if write!(&mut output, "{byte:02x}").is_err() {
            break;
        }
    }
    output
}

#[must_use]
pub fn hint_path(event: &CaptureEvent) -> &Path {
    Path::new(&event.vault_hint)
}

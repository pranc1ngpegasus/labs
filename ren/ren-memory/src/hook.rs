use std::{
    env, fs,
    path::{Path, PathBuf},
};

use fs2::FileExt;
use serde::Serialize;
use toml_edit::{Array, DocumentMut, InlineTable, Item, Table, Value};

use crate::{
    error::{MemoryError, Result},
    fsutil::{create_private_dir, open_private_lock, write_atomic_replace},
};

const OWNED_COMMAND: &str = "ren memory ingest-hook --agent codex --event stop --quiet";
const OWNERSHIP_MARKER: &str = "ren-memory-owned:v1";

#[derive(Clone, Debug, Serialize)]
pub struct HookStatus {
    pub agent: String,
    pub scope: String,
    pub installed: bool,
    pub config_path: PathBuf,
    pub command: String,
}

pub fn install_codex_user() -> Result<HookStatus> {
    let path = codex_config_path()?;
    install_at(path)
}

pub fn install_at(path: PathBuf) -> Result<HookStatus> {
    update_document(&path, |document| {
        if contains_owned_hook(document) {
            return Ok(false);
        }
        ensure_stop_array(document)?.push(Value::InlineTable(owned_group()));
        Ok(true)
    })?;
    Ok(status_for(path, true))
}

pub fn status_codex_user() -> Result<HookStatus> {
    let path = codex_config_path()?;
    status_at(path)
}

pub fn status_at(path: PathBuf) -> Result<HookStatus> {
    let installed = if path.exists() {
        contains_owned_hook(&read_document(&path)?)
    } else {
        false
    };
    Ok(status_for(path, installed))
}

pub fn uninstall_codex_user() -> Result<HookStatus> {
    let path = codex_config_path()?;
    uninstall_at(path)
}

pub fn uninstall_at(path: PathBuf) -> Result<HookStatus> {
    if !path.exists() {
        return Ok(status_for(path, false));
    }
    update_document(&path, |document| {
        let changed = contains_owned_hook(document);
        if let Some(array) = document
            .get_mut("hooks")
            .and_then(Item::as_table_like_mut)
            .and_then(|hooks| hooks.get_mut("Stop"))
            .and_then(Item::as_array_mut)
        {
            let retained = array
                .iter()
                .cloned()
                .filter_map(remove_owned_handlers)
                .collect::<Vec<_>>();
            array.clear();
            for value in retained {
                array.push(value);
            }
        }
        Ok(changed)
    })?;
    Ok(status_for(path, false))
}

fn codex_config_path() -> Result<PathBuf> {
    if let Some(path) = env::var_os("CODEX_HOME") {
        return Ok(PathBuf::from(path).join("config.toml"));
    }
    let home = env::var_os("HOME").ok_or(MemoryError::HomeUnavailable)?;
    Ok(PathBuf::from(home).join(".codex").join("config.toml"))
}

fn read_document(path: &Path) -> Result<DocumentMut> {
    let input = read_document_text(path)?;
    parse_document(path, &input)
}

fn parse_document(
    path: &Path,
    input: &str,
) -> Result<DocumentMut> {
    if input.is_empty() {
        return Ok(DocumentMut::new());
    }
    input.parse::<DocumentMut>().map_err(|error| {
        MemoryError::InvalidConfig(format!("cannot parse {}: {error}", path.display()))
    })
}

fn read_document_text(path: &Path) -> Result<String> {
    match fs::read_to_string(path) {
        Ok(input) => Ok(input),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(MemoryError::io(path, error)),
    }
}

fn update_document(
    path: &Path,
    mutator: impl Fn(&mut DocumentMut) -> Result<bool>,
) -> Result<()> {
    let lock_path = path.with_extension("toml.ren-memory.lock");
    let lock = open_private_lock(&lock_path)?;
    lock.lock_exclusive()
        .map_err(|error| MemoryError::io(&lock_path, error))?;
    for _ in 0..3 {
        let original = read_document_text(path)?;
        let mut document = parse_document(path, &original)?;
        if !mutator(&mut document)? {
            return Ok(());
        }
        if read_document_text(path)? != original {
            continue;
        }
        save_document(path, &document)?;
        return Ok(());
    }
    Err(MemoryError::Validation(format!(
        "Codex configuration changed repeatedly while updating {}",
        path.display()
    )))
}

fn save_document(
    path: &Path,
    document: &DocumentMut,
) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| MemoryError::InvalidConfig("Codex config has no parent".into()))?;
    create_private_dir(parent)?;
    write_atomic_replace(path, document.to_string().as_bytes())
}

fn ensure_stop_array(document: &mut DocumentMut) -> Result<&mut Array> {
    if document.get("hooks").is_none() {
        document["hooks"] = Item::Table(Table::new());
    }
    let hooks = document["hooks"].as_table_like_mut().ok_or_else(|| {
        MemoryError::InvalidConfig("Codex `hooks` configuration must be a table".into())
    })?;
    if hooks.get("Stop").is_none() {
        hooks.insert("Stop", Item::Value(Value::Array(Array::new())));
    }
    hooks
        .get_mut("Stop")
        .and_then(Item::as_array_mut)
        .ok_or_else(|| {
            MemoryError::InvalidConfig("Codex `hooks.Stop` configuration must be an array".into())
        })
}

fn owned_group() -> InlineTable {
    let mut handler = InlineTable::new();
    handler.insert("type", Value::from("command"));
    handler.insert("command", Value::from(OWNED_COMMAND));
    handler.insert("timeout", Value::from(5));
    handler.insert("statusMessage", Value::from(OWNERSHIP_MARKER));
    let mut handlers = Array::new();
    handlers.push(Value::InlineTable(handler));

    let mut group = InlineTable::new();
    group.insert("matcher", Value::from(""));
    group.insert("hooks", Value::Array(handlers));
    group
}

fn contains_owned_hook(document: &DocumentMut) -> bool {
    document
        .get("hooks")
        .and_then(Item::as_table_like)
        .and_then(|hooks| hooks.get("Stop"))
        .and_then(Item::as_array)
        .is_some_and(|groups| groups.iter().any(is_owned_group))
}

fn is_owned_group(value: &Value) -> bool {
    value
        .as_inline_table()
        .and_then(|group| group.get("hooks"))
        .and_then(Value::as_array)
        .is_some_and(|handlers| {
            handlers.iter().any(|handler| {
                handler.as_inline_table().is_some_and(|handler| {
                    handler
                        .get("command")
                        .and_then(Value::as_str)
                        .is_some_and(|command| command == OWNED_COMMAND)
                        && handler
                            .get("statusMessage")
                            .and_then(Value::as_str)
                            .is_some_and(|marker| marker == OWNERSHIP_MARKER)
                })
            })
        })
}

fn remove_owned_handlers(mut value: Value) -> Option<Value> {
    let Some(group) = value.as_inline_table_mut() else {
        return Some(value);
    };
    let Some(handlers) = group.get_mut("hooks").and_then(Value::as_array_mut) else {
        return Some(value);
    };
    let retained = handlers
        .iter()
        .filter(|handler| !is_owned_handler(handler))
        .cloned()
        .collect::<Vec<_>>();
    handlers.clear();
    for handler in retained {
        handlers.push(handler);
    }
    if handlers.is_empty() {
        None
    } else {
        Some(value)
    }
}

fn is_owned_handler(handler: &Value) -> bool {
    handler.as_inline_table().is_some_and(|handler| {
        handler
            .get("command")
            .and_then(Value::as_str)
            .is_some_and(|command| command == OWNED_COMMAND)
            && handler
                .get("statusMessage")
                .and_then(Value::as_str)
                .is_some_and(|marker| marker == OWNERSHIP_MARKER)
    })
}

fn status_for(
    config_path: PathBuf,
    installed: bool,
) -> HookStatus {
    HookStatus {
        agent: "codex".into(),
        scope: "user".into(),
        installed,
        config_path,
        command: OWNED_COMMAND.into(),
    }
}

//! Smoke: call `code_search` and `bash` through [`sui_tools::builtin_registry`].
//!
//! ```text
//! cargo run -p sui-tools --example smoke_builtin -- /path/to/repo
//! ```

use std::{env, path::PathBuf};

use serde_json::json;
use sui_tools::{Bm25Index, ToolsError, builtin_registry};

#[tokio::main]
async fn main() -> Result<(), ToolsError> {
    let root = match env::args().nth(1) {
        Some(path) => PathBuf::from(path),
        None => env::current_dir().map_err(|source| ToolsError::io(".", source))?,
    };

    println!("indexing {} …", root.display());
    let index = Bm25Index::index_tree(&root, &["rs", "toml", "md", "rhai"])?;
    println!("indexed {} documents", index.len());

    let registry = builtin_registry(index, Some(&root))?;
    println!("tools: {:?}", registry.names());
    println!(
        "descriptors: {}",
        serde_json::to_string_pretty(&registry.descriptors())?
    );

    let search = registry
        .call(
            "code_search",
            json!({ "query": "Bm25Index builtin_registry", "limit": 5 }),
        )
        .await?;
    println!(
        "\n=== code_search ===\n{}",
        serde_json::to_string_pretty(&search)?
    );

    let hits = search["hits"]
        .as_array()
        .ok_or_else(|| ToolsError::Search("missing hits".into()))?;
    if hits.is_empty() {
        return Err(ToolsError::Search(
            "expected at least one hit for Bm25Index".into(),
        ));
    }

    let ran = registry
        .call(
            "bash",
            json!({
                "command": "printf 'smoke-ok %s\\n' \"$(basename \"$PWD\")\""
            }),
        )
        .await?;
    println!(
        "\n=== bash run ===\n{}",
        serde_json::to_string_pretty(&ran)?
    );
    let stdout = ran["stdout"].as_str().unwrap_or_default();
    if !stdout.contains("smoke-ok") {
        return Err(ToolsError::Bash(format!(
            "expected smoke-ok in stdout, got {stdout:?}"
        )));
    }

    println!("\nsmoke_builtin: OK");
    Ok(())
}

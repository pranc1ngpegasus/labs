//! Benchmark [`TantivyIndex::index_tree`] cold build, reopen + query latency.
//!
//! Optional RSS: wrap with `/usr/bin/time -v` (Linux) or `/usr/bin/time -l` (macOS).
//!
//! ```text
//! cargo build -p sui-tools --example bench_tantivy --release
//! /usr/bin/time -v ./target/release/examples/bench_tantivy ~/.cargo/registry/src /tmp/sui-tv-index
//! ```

use std::{env, path::PathBuf, time::Instant};

use sui_tools::{LexicalSearch, MAX_INDEX_DOCS, TantivyIndex, ToolsError};

fn rss_kb() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            let kb = rest.split_whitespace().next()?.parse().ok()?;
            return Some(kb);
        }
    }
    None
}

fn main() -> Result<(), ToolsError> {
    let root = env::args().nth(1).map_or_else(
        || env::current_dir().map_err(|source| ToolsError::io(".", source)),
        |path| Ok(PathBuf::from(path)),
    )?;
    let index_dir = env::args().nth(2).map_or_else(
        || env::temp_dir().join(format!("sui-tools-tantivy-bench-{}", std::process::id())),
        PathBuf::from,
    );
    let extensions: Vec<String> = env::args().nth(3).map_or_else(
        || vec!["rs".into(), "toml".into(), "md".into()],
        |list| list.split(',').map(str::to_owned).collect(),
    );
    let ext_refs: Vec<&str> = extensions.iter().map(String::as_str).collect();
    let query = env::args()
        .nth(4)
        .unwrap_or_else(|| "HashMap async spawn".into());

    println!("root={}", root.display());
    println!("index_dir={}", index_dir.display());
    println!("extensions={extensions:?}");
    println!("MAX_INDEX_DOCS={MAX_INDEX_DOCS}");

    let started = Instant::now();
    let index = TantivyIndex::index_tree(&index_dir, &root, &ext_refs)?;
    let cold_elapsed = started.elapsed();
    let capped = index.len() >= MAX_INDEX_DOCS;
    println!("documents={}", index.len());
    println!("capped_at_max_docs={capped}");
    println!("cold_index_secs={:.3}", cold_elapsed.as_secs_f64());
    println!("cold_index_ms={}", cold_elapsed.as_millis());
    if let Some(kb) = rss_kb() {
        println!("rss_kb_after_cold_build={kb}");
    }
    drop(index);

    let reopen_started = Instant::now();
    let index = TantivyIndex::open(&index_dir)?;
    let reopen_elapsed = reopen_started.elapsed();
    println!("reopen_ms={}", reopen_elapsed.as_millis());
    println!("reopen_us={}", reopen_elapsed.as_micros());
    if let Some(kb) = rss_kb() {
        println!("rss_kb_after_reopen={kb}");
    }

    let search_started = Instant::now();
    let hits = LexicalSearch::search(&index, &query, 10);
    let search_elapsed = search_started.elapsed();
    println!("query={query:?}");
    println!("search_ms={}", search_elapsed.as_millis());
    println!("search_us={}", search_elapsed.as_micros());
    for (i, hit) in hits.iter().enumerate() {
        println!("hit[{i}] score={:.4} path={}", hit.score, hit.path);
    }

    // Keep the index alive until process exit so RSS reflects resident mmap/index.
    std::mem::forget(index);
    if let Some(kb) = rss_kb() {
        println!("rss_kb_final={kb}");
    }
    Ok(())
}

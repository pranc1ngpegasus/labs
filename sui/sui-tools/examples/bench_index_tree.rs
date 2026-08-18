//! Benchmark [`Bm25Index::index_tree`] wall time (and optional RSS via `/proc` or `/usr/bin/time -l`).
//!
//! ```text
//! cargo build -p sui-tools --example bench_index_tree --release
//! /usr/bin/time -l ./target/release/examples/bench_index_tree ~/.cargo/registry/src
//! ```

use std::{env, path::PathBuf, time::Instant};

use sui_tools::{Bm25Index, MAX_INDEX_DOCS, ToolsError};

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
    let extensions: Vec<String> = env::args().nth(2).map_or_else(
        || vec!["rs".into(), "toml".into(), "md".into()],
        |list| list.split(',').map(str::to_owned).collect(),
    );
    let ext_refs: Vec<&str> = extensions.iter().map(String::as_str).collect();

    println!("root={}", root.display());
    println!("extensions={extensions:?}");
    println!("MAX_INDEX_DOCS={MAX_INDEX_DOCS}");

    let started = Instant::now();
    let index = Bm25Index::index_tree(&root, &ext_refs)?;
    let elapsed = started.elapsed();

    let capped = index.len() >= MAX_INDEX_DOCS;
    println!("documents={}", index.len());
    println!("capped_at_max_docs={capped}");
    println!("index_secs={:.3}", elapsed.as_secs_f64());
    println!("index_ms={}", elapsed.as_millis());
    if let Some(kb) = rss_kb() {
        println!("rss_kb_after_index={kb}");
    }

    let query = env::args()
        .nth(3)
        .unwrap_or_else(|| "HashMap async spawn".into());
    let search_started = Instant::now();
    let hits = index.search(&query, 10);
    let search_elapsed = search_started.elapsed();
    println!("query={query:?}");
    println!("search_ms={}", search_elapsed.as_millis());
    println!("search_us={}", search_elapsed.as_micros());
    for (i, hit) in hits.iter().enumerate() {
        println!("hit[{i}] score={:.4} path={}", hit.score, hit.path);
    }

    // Keep the index alive until process exit so RSS reflects resident index size.
    std::mem::forget(index);
    if let Some(kb) = rss_kb() {
        println!("rss_kb_final={kb}");
    }
    Ok(())
}

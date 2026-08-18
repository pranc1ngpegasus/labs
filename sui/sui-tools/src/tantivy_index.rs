//! Tantivy-backed persistent lexical code search.
//!
//! # Tokenization tradeoff
//!
//! Rather than a custom Tantivy tokenizer, documents are **preprocessed** with
//! [`crate::tokenize`] (camelCase / `snake_case` splitting) and indexed as a
//! whitespace-separated token stream under Tantivy's default text analyzer.
//! Queries use the same preprocessing and a disjunctive (`Should`) boolean of
//! term queries so ranking stays BM25-ish and OR-of-terms like [`crate::Bm25Index`].
//!
//! **Pros:** reuses the proven code tokenizer; no Tantivy tokenizer plugin
//! surface; easy to reason about.
//! **Cons:** original source positions are lost (no phrase queries over raw
//! code); snippets come from a stored preview, not highlighter offsets.

use std::{fs, path::Path};

use tantivy::{
    Index, IndexReader, ReloadPolicy, TantivyDocument, Term,
    collector::TopDocs,
    query::{BooleanQuery, Occur, Query, TermQuery},
    schema::{Field, IndexRecordOption, STORED, STRING, Schema, TEXT, Value},
};

use crate::{
    LexicalSearch, SearchHit, ToolsError,
    bm25::unique_tokens,
    corpus::{MAX_INDEX_DOCS, VisitControl, make_snippet, visit_code_files},
    tokenize,
};

/// Heap budget passed to [`Index::writer`] while building a tree index.
const WRITER_HEAP_BYTES: usize = 50_000_000;

/// On-disk Tantivy index for local code corpora.
pub struct TantivyIndex {
    reader: IndexReader,
    path_field: Field,
    body_field: Field,
    snippet_field: Field,
    doc_count: usize,
}

impl TantivyIndex {
    /// Opens an existing index directory (no re-index).
    ///
    /// # Errors
    ///
    /// Returns [`ToolsError::Search`] when the directory is not a readable
    /// Tantivy index, or [`ToolsError::Io`] for directory I/O failures.
    pub fn open(index_dir: &Path) -> Result<Self, ToolsError> {
        let index = Index::open_in_dir(index_dir).map_err(search_err)?;
        Self::from_index(&index)
    }

    /// Builds a fresh index under `index_dir` from `root`.
    ///
    /// **Destructive:** if `index_dir` already exists it is removed entirely
    /// (`remove_dir_all`) before creating the new index. Pass a dedicated
    /// index path (not a source tree).
    ///
    /// Uses the same corpus policy as [`crate::Bm25Index::index_tree`].
    /// Stops once [`MAX_INDEX_DOCS`] documents are added.
    /// Files that tokenize to an empty body are skipped (hence document counts
    /// may be slightly lower than in-memory BM25 on the same tree).
    ///
    /// # Errors
    ///
    /// Propagates I/O and Tantivy errors as [`ToolsError`].
    pub fn index_tree(
        index_dir: &Path,
        root: &Path,
        extensions: &[&str],
    ) -> Result<Self, ToolsError> {
        if index_dir.exists() {
            fs::remove_dir_all(index_dir).map_err(|source| ToolsError::io(index_dir, source))?;
        }
        fs::create_dir_all(index_dir).map_err(|source| ToolsError::io(index_dir, source))?;

        let (schema, path_field, body_field, snippet_field) = build_schema();
        let index = Index::create_in_dir(index_dir, schema).map_err(search_err)?;
        let mut writer = index.writer(WRITER_HEAP_BYTES).map_err(search_err)?;

        let mut doc_count = 0usize;
        visit_code_files(root, extensions, |path, text| {
            if doc_count >= MAX_INDEX_DOCS {
                return Ok(VisitControl::Stop);
            }
            let path_str = path.to_string_lossy();
            let body = code_body_for_index(text);
            if body.is_empty() {
                return Ok(VisitControl::Continue);
            }
            let snippet = make_snippet(text);
            let mut document = TantivyDocument::default();
            document.add_text(path_field, path_str.as_ref());
            document.add_text(body_field, body.as_str());
            document.add_text(snippet_field, snippet.as_str());
            writer.add_document(document).map_err(search_err)?;
            doc_count = doc_count.saturating_add(1);
            Ok(VisitControl::Continue)
        })?;

        writer.commit().map_err(search_err)?;
        drop(writer);

        let mut opened = Self::from_index(&index)?;
        opened.doc_count = doc_count;
        Ok(opened)
    }

    fn from_index(index: &Index) -> Result<Self, ToolsError> {
        let schema = index.schema();
        let path_field = schema.get_field("path").map_err(search_err)?;
        let body_field = schema.get_field("body").map_err(search_err)?;
        let snippet_field = schema.get_field("snippet").map_err(search_err)?;
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()
            .map_err(search_err)?;
        reader.reload().map_err(search_err)?;
        let doc_count = usize::try_from(reader.searcher().num_docs()).unwrap_or(usize::MAX);
        Ok(Self {
            reader,
            path_field,
            body_field,
            snippet_field,
            doc_count,
        })
    }

    /// Number of documents known after the last open / build.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.doc_count
    }

    /// Returns `true` when no documents are indexed.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.doc_count == 0
    }

    /// Reloads the reader (useful after external writers commit).
    ///
    /// # Errors
    ///
    /// Returns [`ToolsError::Search`] on Tantivy reload failure.
    pub fn reload(&mut self) -> Result<(), ToolsError> {
        self.reader.reload().map_err(search_err)?;
        self.doc_count = usize::try_from(self.reader.searcher().num_docs()).unwrap_or(usize::MAX);
        Ok(())
    }

    /// Searches with BM25 scoring over pre-tokenized body terms.
    ///
    /// On Tantivy query/collector failure this returns an empty `Vec` (same
    /// infallible shape as [`crate::Bm25Index::search`] / [`LexicalSearch`]).
    #[must_use]
    pub fn search(
        &self,
        query: &str,
        limit: usize,
    ) -> Vec<SearchHit> {
        if self.is_empty() || limit == 0 {
            return Vec::new();
        }
        let terms = unique_tokens(query);
        if terms.is_empty() {
            return Vec::new();
        }

        let mut clauses: Vec<(Occur, Box<dyn Query>)> = Vec::with_capacity(terms.len());
        for term in terms {
            let tq = TermQuery::new(
                Term::from_field_text(self.body_field, &term),
                IndexRecordOption::WithFreqs,
            );
            clauses.push((Occur::Should, Box::new(tq)));
        }
        let boolean = BooleanQuery::new(clauses);
        let searcher = self.reader.searcher();
        let Ok(top_docs) = searcher.search(&boolean, &TopDocs::with_limit(limit).order_by_score())
        else {
            return Vec::new();
        };

        let mut hits = Vec::with_capacity(top_docs.len());
        for (score, addr) in top_docs {
            let Ok(doc) = searcher.doc::<TantivyDocument>(addr) else {
                continue;
            };
            let path = doc
                .get_first(self.path_field)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_owned();
            let snippet = doc
                .get_first(self.snippet_field)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_owned();
            hits.push(SearchHit {
                doc_id: path.clone(),
                path,
                score: f64::from(score),
                snippet,
            });
        }
        // Stable tie-break by path (Tantivy already sorts by score).
        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.path.cmp(&b.path))
        });
        hits
    }
}

impl LexicalSearch for TantivyIndex {
    fn search(
        &self,
        query: &str,
        limit: usize,
    ) -> Vec<SearchHit> {
        Self::search(self, query, limit)
    }
}

fn build_schema() -> (Schema, Field, Field, Field) {
    let mut builder = Schema::builder();
    let path_field = builder.add_text_field("path", STRING | STORED);
    // TEXT: tokenized + freqs for BM25. Body is already code-preprocessed.
    let body_field = builder.add_text_field("body", TEXT);
    let snippet_field = builder.add_text_field("snippet", STORED);
    (builder.build(), path_field, body_field, snippet_field)
}

/// Joins [`tokenize`] output so Tantivy's default analyzer indexes code subtokens.
fn code_body_for_index(text: &str) -> String {
    tokenize(text).join(" ")
}

fn search_err(error: impl std::fmt::Display) -> ToolsError {
    ToolsError::Search(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::corpus::{TempDir, temp_dir};
    use std::fs;

    #[test]
    fn ranks_and_persists_across_reopen() -> Result<(), ToolsError> {
        let src = TempDir(temp_dir("tv-src"));
        let idx_dir = TempDir(temp_dir("tv-idx"));
        fs::create_dir_all(src.0.join("src")).map_err(|source| ToolsError::io(&src.0, source))?;
        fs::write(
            src.0.join("src/auth.rs"),
            "fn authenticate_user(password: &str) { verify_password(password) }",
        )
        .map_err(|source| ToolsError::io(src.0.join("src/auth.rs"), source))?;
        fs::write(
            src.0.join("src/render.rs"),
            "fn render_widget(layout: Layout) { draw_frame(layout) }",
        )
        .map_err(|source| ToolsError::io(src.0.join("src/render.rs"), source))?;
        fs::write(
            src.0.join("src/password.rs"),
            "fn hash_password(password: &str) -> Digest { blake3(password) }",
        )
        .map_err(|source| ToolsError::io(src.0.join("src/password.rs"), source))?;

        let built = TantivyIndex::index_tree(&idx_dir.0, &src.0, &["rs"])?;
        assert_eq!(built.len(), 3);
        let hits = LexicalSearch::search(&built, "password authenticate", 3);
        assert_eq!(hits.len(), 2);
        assert!(hits[0].path.ends_with("auth.rs"));
        assert!(hits[0].score > hits[1].score);
        assert!(!hits[0].snippet.is_empty());
        drop(built);

        let reopened = TantivyIndex::open(&idx_dir.0)?;
        assert_eq!(reopened.len(), 3);
        let hits = reopened.search("password authenticate", 3);
        assert_eq!(hits.len(), 2);
        assert!(hits[0].path.ends_with("auth.rs"));
        Ok(())
    }

    #[test]
    fn camel_case_query_matches_preprocessed_body() -> Result<(), ToolsError> {
        let src = TempDir(temp_dir("tv-camel-src"));
        let idx_dir = TempDir(temp_dir("tv-camel-idx"));
        fs::create_dir_all(src.0.join("src")).map_err(|source| ToolsError::io(&src.0, source))?;
        fs::write(src.0.join("src/lib.rs"), "fn parseHttpResponse() {}")
            .map_err(|source| ToolsError::io(src.0.join("src/lib.rs"), source))?;

        let index = TantivyIndex::index_tree(&idx_dir.0, &src.0, &["rs"])?;
        let hits = index.search("parseHttp", 5);
        assert_eq!(hits.len(), 1);
        assert!(hits[0].path.ends_with("lib.rs"));
        Ok(())
    }

    #[test]
    fn empty_query_returns_no_hits() -> Result<(), ToolsError> {
        let src = TempDir(temp_dir("tv-empty-src"));
        let idx_dir = TempDir(temp_dir("tv-empty-idx"));
        fs::create_dir_all(src.0.join("src")).map_err(|source| ToolsError::io(&src.0, source))?;
        fs::write(src.0.join("src/lib.rs"), "fn hello() {}")
            .map_err(|source| ToolsError::io(src.0.join("src/lib.rs"), source))?;
        let index = TantivyIndex::index_tree(&idx_dir.0, &src.0, &["rs"])?;
        assert!(index.search("", 10).is_empty());
        assert!(index.search("hello", 0).is_empty());
        Ok(())
    }
}

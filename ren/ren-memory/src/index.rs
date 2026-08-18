use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque},
    fs,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use ignore::{DirEntry, WalkBuilder};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::{
    error::{MemoryError, Result},
    fsutil::create_private_dir,
    model::{Note, NoteState, NoteType, read_note},
    vault::Vault,
};

#[derive(Clone, Debug, Default, Serialize)]
pub struct SyncReport {
    pub indexed: usize,
    pub unchanged: usize,
    pub removed: usize,
    pub invalid: Vec<Diagnostic>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Diagnostic {
    pub class: String,
    pub path: PathBuf,
    pub message: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct NoteSummary {
    pub id: String,
    pub path: PathBuf,
    pub note_type: String,
    pub state: String,
    pub title: Option<String>,
    pub created_at: String,
    pub updated_at: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SearchHit {
    pub id: String,
    pub note_type: String,
    pub state: String,
    pub title: Option<String>,
    pub snippet: String,
    pub score: f64,
}

#[derive(Clone, Debug, Serialize)]
pub struct Edge {
    pub from: String,
    pub to: String,
    pub relation: String,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct DoctorReport {
    pub ok: bool,
    pub sqlite_version: String,
    pub sqlite_version_supported: bool,
    pub journal_mode: String,
    pub schema_version: i64,
    pub notes_on_disk: usize,
    pub indexed_notes: usize,
    pub diagnostics: Vec<DoctorDiagnostic>,
}

#[derive(Clone, Debug, Serialize)]
pub struct DoctorDiagnostic {
    pub class: String,
    pub severity: String,
    pub note_id: Option<String>,
    pub path: Option<PathBuf>,
    pub message: String,
}

#[derive(Debug)]
struct IndexedFile {
    path: PathBuf,
    relative_path: String,
    size: u64,
    modified_ns: i64,
    hash: Vec<u8>,
    note: Note,
}

#[derive(Debug)]
struct RevisionSnapshot {
    note_id: String,
    content_hash: Vec<u8>,
    recorded_at: String,
    markdown: String,
}

#[derive(Debug)]
enum ChangedFile {
    Valid(Box<IndexedFile>),
    Invalid {
        relative_path: String,
        diagnostic: Diagnostic,
    },
}

#[allow(clippy::too_many_lines)]
pub fn sync(
    vault: &Vault,
    rebuild: bool,
    blocking: bool,
) -> Result<SyncReport> {
    let _lock = vault.lock_writer(blocking)?;
    let mut connection = open_writer(vault)?;
    crate::mutation::recover_transactions_locked(vault, &connection)?;
    if rebuild {
        clear_projection(&mut connection)?;
    }

    let disk_paths = note_paths(vault)?;
    let indexed_state = load_index_state(&connection)?;
    let diagnostic_paths = load_diagnostic_paths(&connection)?;
    let mut changed = Vec::new();
    let mut report = SyncReport::default();
    let mut current_paths = BTreeSet::new();
    let mut metadata_updates = Vec::new();

    for path in disk_paths {
        let relative_path = relative_path(vault, &path)?;
        current_paths.insert(relative_path.clone());
        let metadata =
            fs::symlink_metadata(&path).map_err(|error| MemoryError::io(&path, error))?;
        if metadata.file_type().is_symlink() {
            let diagnostic = Diagnostic {
                class: "symlink_rejected".into(),
                path: path.clone(),
                message: "managed notes must not be symlinks".into(),
            };
            report.invalid.push(diagnostic.clone());
            changed.push(ChangedFile::Invalid {
                relative_path,
                diagnostic,
            });
            continue;
        }
        let size = metadata.len();
        let modified_ns = modified_ns(&metadata);
        let input = match fs::read_to_string(&path) {
            Ok(input) => input,
            Err(error) => {
                let diagnostic = Diagnostic {
                    class: "read_error".into(),
                    path: path.clone(),
                    message: error.to_string(),
                };
                report.invalid.push(diagnostic.clone());
                changed.push(ChangedFile::Invalid {
                    relative_path,
                    diagnostic,
                });
                continue;
            },
        };
        let hash = Sha256::digest(input.as_bytes()).to_vec();
        if indexed_state
            .get(&relative_path)
            .is_some_and(|state| state.hash == hash)
        {
            report.unchanged += 1;
            if indexed_state
                .get(&relative_path)
                .is_some_and(|state| state.size != size || state.modified_ns != modified_ns)
            {
                metadata_updates.push((relative_path, size, modified_ns));
            }
            continue;
        }
        match Note::parse(&path, &input) {
            Ok(note) => changed.push(ChangedFile::Valid(Box::new(IndexedFile {
                path,
                relative_path,
                size,
                modified_ns,
                hash,
                note,
            }))),
            Err(error) => {
                let diagnostic = Diagnostic {
                    class: error.class().into(),
                    path: path.clone(),
                    message: error.to_string(),
                };
                report.invalid.push(diagnostic.clone());
                changed.push(ChangedFile::Invalid {
                    relative_path,
                    diagnostic,
                });
            },
        }
    }

    let removed = indexed_state
        .keys()
        .chain(diagnostic_paths.iter())
        .filter(|path| !current_paths.contains(*path))
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let removed_set = removed.iter().map(String::as_str).collect::<HashSet<_>>();
    validate_changed_ids(&connection, &changed, &removed_set)?;
    report.removed = removed.len();
    report.indexed = changed
        .iter()
        .filter(|file| matches!(file, ChangedFile::Valid(_)))
        .count();
    let revisions = load_revision_snapshots(vault)?;

    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    for path in removed {
        delete_by_path(&transaction, &path)?;
    }
    for (path, size, modified_ns) in metadata_updates {
        transaction.execute(
            "UPDATE index_state SET size = ?2, modified_ns = ?3 WHERE path = ?1",
            params![path, i64::try_from(size).unwrap_or(i64::MAX), modified_ns],
        )?;
        transaction.execute(
            "UPDATE nodes SET modified_ns = ?2 WHERE path = ?1",
            params![path, modified_ns],
        )?;
    }
    for file in changed {
        match file {
            ChangedFile::Valid(file) => upsert_file(&transaction, &file)?,
            ChangedFile::Invalid {
                relative_path,
                diagnostic,
            } => {
                delete_by_path(&transaction, &relative_path)?;
                upsert_diagnostic(&transaction, &relative_path, &diagnostic)?;
            },
        }
    }
    replace_revisions(&transaction, &revisions)?;
    transaction.commit()?;
    write_diagnostics(vault, &report.invalid)?;
    crate::capture::reconcile_receipts(vault)?;
    Ok(report)
}

pub fn list(
    vault: &Vault,
    note_type: Option<NoteType>,
    state: Option<NoteState>,
) -> Result<Vec<NoteSummary>> {
    let connection = open_read(vault)?;
    let mut statement = connection.prepare(
        "SELECT id, path, note_type, state, title, created_at, updated_at
         FROM nodes
         WHERE (?1 IS NULL OR note_type = ?1)
           AND (?2 IS NULL OR state = ?2)
         ORDER BY created_at, id",
    )?;
    let note_type = note_type.map(|value| value.to_string());
    let state = state.map(|value| value.to_string());
    let rows = statement.query_map(params![note_type, state], |row| {
        Ok(NoteSummary {
            id: row.get(0)?,
            path: vault.root.join(row.get::<_, String>(1)?),
            note_type: row.get(2)?,
            state: row.get(3)?,
            title: row.get(4)?,
            created_at: row.get(5)?,
            updated_at: row.get(6)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(MemoryError::from)
}

pub fn search(
    vault: &Vault,
    query: &str,
    limit: usize,
) -> Result<Vec<SearchHit>> {
    if query.trim().is_empty() {
        return Err(MemoryError::Validation(
            "search query must not be empty".into(),
        ));
    }
    let connection = open_read(vault)?;
    let mut hits = search_fts(&connection, query, limit)?;
    let substring_hits = search_substring(&connection, query, limit)?;
    for candidate in substring_hits {
        if let Some(existing) = hits.iter_mut().find(|hit| hit.id == candidate.id) {
            if candidate.score > existing.score {
                existing.score = candidate.score;
            }
        } else {
            hits.push(candidate);
        }
    }
    hits.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.id.cmp(&right.id))
    });
    hits.truncate(limit);
    Ok(hits)
}

pub fn note_path(
    vault: &Vault,
    id: &str,
) -> Result<PathBuf> {
    let connection = open_read(vault)?;
    let path = connection
        .query_row("SELECT path FROM nodes WHERE id = ?1", [id], |row| {
            row.get::<_, String>(0)
        })
        .optional()?
        .ok_or_else(|| MemoryError::NoteNotFound(id.into()))?;
    let path = vault.root.join(path);
    ensure_inside_vault(vault, &path)?;
    Ok(path)
}

pub fn edges_from(
    vault: &Vault,
    id: &str,
) -> Result<Vec<Edge>> {
    edges(vault, "from_id", id)
}

pub fn edges_to(
    vault: &Vault,
    id: &str,
) -> Result<Vec<Edge>> {
    edges(vault, "to_id", id)
}

pub fn backlinks(
    vault: &Vault,
    id: &str,
) -> Result<Vec<Edge>> {
    edges_to(vault, id)
}

pub fn related(
    vault: &Vault,
    id: &str,
    depth: usize,
) -> Result<Vec<Edge>> {
    ensure_note_exists(vault, id)?;
    let connection = open_read(vault)?;
    let all = all_knowledge_edges(&connection)?;
    let mut frontier = VecDeque::from([(id.to_owned(), 0_usize)]);
    let mut visited = HashSet::from([id.to_owned()]);
    let mut result = Vec::new();
    let mut seen_edges = HashSet::new();
    while let Some((current, current_depth)) = frontier.pop_front() {
        if current_depth >= depth {
            continue;
        }
        for edge in all
            .iter()
            .filter(|edge| edge.from == current || edge.to == current)
        {
            let key = (edge.from.clone(), edge.to.clone(), edge.relation.clone());
            if seen_edges.insert(key) {
                result.push(edge.clone());
            }
            let neighbor = if edge.from == current {
                &edge.to
            } else {
                &edge.from
            };
            if visited.insert(neighbor.clone()) {
                frontier.push_back((neighbor.clone(), current_depth + 1));
            }
        }
    }
    Ok(result)
}

pub fn shortest_path(
    vault: &Vault,
    from: &str,
    to: &str,
) -> Result<Vec<String>> {
    ensure_note_exists(vault, from)?;
    ensure_note_exists(vault, to)?;
    let connection = open_read(vault)?;
    let edges = all_knowledge_edges(&connection)?;
    let mut frontier = VecDeque::from([from.to_owned()]);
    let mut previous = HashMap::<String, String>::new();
    let mut visited = HashSet::from([from.to_owned()]);
    while let Some(current) = frontier.pop_front() {
        if current == to {
            break;
        }
        for edge in edges
            .iter()
            .filter(|edge| edge.from == current || edge.to == current)
        {
            let neighbor = if edge.from == current {
                &edge.to
            } else {
                &edge.from
            };
            if visited.insert(neighbor.clone()) {
                previous.insert(neighbor.clone(), current.clone());
                frontier.push_back(neighbor.clone());
            }
        }
    }
    if !visited.contains(to) {
        return Ok(Vec::new());
    }
    let mut path = vec![to.to_owned()];
    let mut current = to;
    while current != from {
        let parent = previous.get(current).ok_or_else(|| {
            MemoryError::Validation("path reconstruction failed unexpectedly".into())
        })?;
        path.push(parent.clone());
        current = parent;
    }
    path.reverse();
    Ok(path)
}

pub fn orphans(vault: &Vault) -> Result<Vec<NoteSummary>> {
    let connection = open_read(vault)?;
    let mut statement = connection.prepare(
        "SELECT id, path, note_type, state, title, created_at, updated_at
         FROM nodes
         WHERE state != 'archived'
           AND NOT EXISTS (SELECT 1 FROM edges WHERE from_id = nodes.id)
           AND NOT EXISTS (SELECT 1 FROM edges WHERE to_id = nodes.id)
         ORDER BY created_at, id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(NoteSummary {
            id: row.get(0)?,
            path: vault.root.join(row.get::<_, String>(1)?),
            note_type: row.get(2)?,
            state: row.get(3)?,
            title: row.get(4)?,
            created_at: row.get(5)?,
            updated_at: row.get(6)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(MemoryError::from)
}

pub fn all_notes(vault: &Vault) -> Result<Vec<NoteSummary>> {
    list(vault, None, None)
}

pub fn all_edges(vault: &Vault) -> Result<Vec<Edge>> {
    let connection = open_read(vault)?;
    let mut edges = all_knowledge_edges(&connection)?;
    edges.extend(dependency_edges(&connection)?);
    edges.sort_by(|left, right| {
        (&left.from, &left.to, &left.relation).cmp(&(&right.from, &right.to, &right.relation))
    });
    Ok(edges)
}

pub fn validate_dependency_graph(vault: &Vault) -> Result<()> {
    let connection = open_read(vault)?;
    let unresolved = connection
        .query_row(
            "SELECT edges.to_id
             FROM edges
             LEFT JOIN nodes AS target ON target.id = edges.to_id
             WHERE edges.relation = 'depends_on' AND target.id IS NULL
             ORDER BY edges.to_id LIMIT 1",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    if let Some(id) = unresolved {
        return Err(MemoryError::Validation(format!(
            "local dependency `{id}` does not resolve"
        )));
    }
    let cycle = connection
        .query_row(
            "WITH RECURSIVE dependency_path(start_id, current_id) AS (
                 SELECT from_id, to_id FROM edges WHERE relation = 'depends_on'
                 UNION
                 SELECT dependency_path.start_id, edges.to_id
                 FROM dependency_path
                 JOIN edges ON edges.from_id = dependency_path.current_id
                 WHERE edges.relation = 'depends_on'
             )
             SELECT start_id
             FROM dependency_path
             WHERE start_id = current_id
             ORDER BY start_id LIMIT 1",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    if let Some(id) = cycle {
        return Err(MemoryError::Validation(format!(
            "local dependency graph contains a cycle through `{id}`"
        )));
    }
    Ok(())
}

pub fn capture_for_event(
    vault: &Vault,
    event_key: &str,
) -> Result<Option<String>> {
    let connection = open_read(vault)?;
    connection
        .query_row(
            "SELECT note_id FROM captures WHERE event_key = ?1",
            [event_key],
            |row| row.get(0),
        )
        .optional()
        .map_err(MemoryError::from)
}

#[allow(clippy::too_many_lines)]
pub fn doctor(vault: &Vault) -> Result<DoctorReport> {
    let connection = match open_read(vault) {
        Ok(connection) => connection,
        Err(error) => return doctor_without_index(vault, &error),
    };
    match connection.query_row("PRAGMA quick_check", [], |row| row.get::<_, String>(0)) {
        Ok(status) if status == "ok" => {},
        Ok(status) => {
            return doctor_without_index(
                vault,
                &MemoryError::Validation(format!("SQLite quick_check failed: {status}")),
            );
        },
        Err(error) => return doctor_without_index(vault, &MemoryError::Sqlite(error)),
    }
    let sqlite_version: String =
        connection.query_row("SELECT sqlite_version()", [], |row| row.get(0))?;
    let journal_mode: String = connection.query_row("PRAGMA journal_mode", [], |row| row.get(0))?;
    let paths = note_paths(vault)?;
    let mut diagnostics = Vec::new();
    let schema_version = match projection_schema_version(&connection) {
        Ok(version) => version,
        Err(error) => {
            diagnostics.push(DoctorDiagnostic {
                class: "schema_inspection".into(),
                severity: "error".into(),
                note_id: None,
                path: Some(vault.database_path()),
                message: format!("cannot inspect disposable index schema: {error}"),
            });
            0
        },
    };
    let schema_supported = schema_version == 1;
    if !schema_supported {
        diagnostics.push(DoctorDiagnostic {
            class: "schema_version".into(),
            severity: "error".into(),
            note_id: None,
            path: Some(vault.database_path()),
            message: if schema_version > 1 {
                format!("index schema version {schema_version} is newer than supported version 1")
            } else {
                format!(
                    "index schema version {schema_version} is incomplete; rebuild the disposable \
                     index"
                )
            },
        });
    }
    let indexed_notes =
        match connection.query_row("SELECT COUNT(*) FROM nodes", [], |row| row.get::<_, i64>(0)) {
            Ok(count) => usize::try_from(count).unwrap_or_default(),
            Err(error) => {
                diagnostics.push(DoctorDiagnostic {
                    class: "schema_inspection".into(),
                    severity: "error".into(),
                    note_id: None,
                    path: Some(vault.database_path()),
                    message: format!("cannot inspect indexed nodes: {error}"),
                });
                0
            },
        };
    let mut notes = Vec::new();
    let mut ids = HashMap::<String, PathBuf>::new();
    for path in &paths {
        match read_note(path) {
            Ok(note) => {
                if let Some(first) = ids.insert(note.frontmatter.id.clone(), path.clone()) {
                    diagnostics.push(DoctorDiagnostic {
                        class: "duplicate_id".into(),
                        severity: "error".into(),
                        note_id: Some(note.frontmatter.id.clone()),
                        path: Some(path.clone()),
                        message: format!("also defined at {}", first.display()),
                    });
                }
                notes.push((path.clone(), note));
            },
            Err(error) => diagnostics.push(DoctorDiagnostic {
                class: error.class().into(),
                severity: "error".into(),
                note_id: None,
                path: Some(path.clone()),
                message: error.to_string(),
            }),
        }
    }
    append_graph_diagnostics(&notes, &mut diagnostics);
    if schema_supported {
        if let Err(error) = append_index_diagnostics(vault, &connection, &notes, &mut diagnostics) {
            diagnostics.push(DoctorDiagnostic {
                class: "schema_inspection".into(),
                severity: "error".into(),
                note_id: None,
                path: Some(vault.database_path()),
                message: format!("cannot inspect index projection: {error}"),
            });
        }
        if let Err(error) =
            append_operational_diagnostics(vault, &connection, &notes, &mut diagnostics)
        {
            diagnostics.push(DoctorDiagnostic {
                class: "schema_inspection".into(),
                severity: "error".into(),
                note_id: None,
                path: Some(vault.database_path()),
                message: format!("cannot inspect index operational state: {error}"),
            });
        }
    }
    let sqlite_version_supported = sqlite_version_at_least(&sqlite_version, (3, 51, 3));
    if !sqlite_version_supported {
        diagnostics.push(DoctorDiagnostic {
            class: "sqlite_version".into(),
            severity: "error".into(),
            note_id: None,
            path: Some(vault.database_path()),
            message: format!("SQLite {sqlite_version} is older than required 3.51.3"),
        });
    }
    if !journal_mode.eq_ignore_ascii_case("wal") {
        diagnostics.push(DoctorDiagnostic {
            class: "journal_mode".into(),
            severity: "error".into(),
            note_id: None,
            path: Some(vault.database_path()),
            message: format!("expected WAL mode, found {journal_mode}"),
        });
    }
    let ok = !diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == "error");
    Ok(DoctorReport {
        ok,
        sqlite_version,
        sqlite_version_supported,
        journal_mode,
        schema_version,
        notes_on_disk: paths.len(),
        indexed_notes,
        diagnostics,
    })
}

fn projection_schema_version(connection: &Connection) -> Result<i64> {
    let value = connection
        .query_row(
            "SELECT value FROM memory_meta WHERE key = 'schema_version'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .ok_or_else(|| {
            MemoryError::Validation("memory_meta does not contain schema_version".into())
        })?;
    value.parse::<i64>().map_err(|error| {
        MemoryError::Validation(format!("schema_version `{value}` is invalid: {error}"))
    })
}

fn doctor_without_index(
    vault: &Vault,
    error: &MemoryError,
) -> Result<DoctorReport> {
    let paths = note_paths(vault)?;
    let mut diagnostics = vec![DoctorDiagnostic {
        class: if vault.database_path().exists() {
            "corrupt_index"
        } else {
            "missing_index"
        }
        .into(),
        severity: "error".into(),
        note_id: None,
        path: Some(vault.database_path()),
        message: format!("the disposable index is unavailable and should be rebuilt: {error}"),
    }];
    let mut notes = Vec::new();
    let mut ids = HashMap::<String, PathBuf>::new();
    for path in &paths {
        match read_note(path) {
            Ok(note) => {
                if let Some(first) = ids.insert(note.frontmatter.id.clone(), path.clone()) {
                    diagnostics.push(DoctorDiagnostic {
                        class: "duplicate_id".into(),
                        severity: "error".into(),
                        note_id: Some(note.frontmatter.id.clone()),
                        path: Some(path.clone()),
                        message: format!("also defined at {}", first.display()),
                    });
                }
                notes.push((path.clone(), note));
            },
            Err(error) => diagnostics.push(DoctorDiagnostic {
                class: error.class().into(),
                severity: "error".into(),
                note_id: None,
                path: Some(path.clone()),
                message: error.to_string(),
            }),
        }
    }
    append_graph_diagnostics(&notes, &mut diagnostics);
    let sqlite_version = rusqlite::version().to_owned();
    Ok(DoctorReport {
        ok: false,
        sqlite_version_supported: sqlite_version_at_least(&sqlite_version, (3, 51, 3)),
        sqlite_version,
        journal_mode: "unavailable".into(),
        schema_version: 0,
        notes_on_disk: paths.len(),
        indexed_notes: 0,
        diagnostics,
    })
}

pub fn open_writer(vault: &Vault) -> Result<Connection> {
    create_private_dir(&vault.index_dir())?;
    ensure_local_database_filesystem(vault)?;
    let connection = Connection::open(vault.database_path())?;
    connection.busy_timeout(std::time::Duration::from_secs(5))?;
    connection.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA foreign_keys = ON;
         PRAGMA synchronous = FULL;
         PRAGMA trusted_schema = OFF;",
    )?;
    create_schema(&connection)?;
    let schema_version = projection_schema_version(&connection)?;
    if schema_version != 1 {
        return Err(MemoryError::InvalidConfig(format!(
            "index schema version {schema_version} is unsupported; supported version: 1"
        )));
    }
    Ok(connection)
}

fn open_read(vault: &Vault) -> Result<Connection> {
    let connection = Connection::open_with_flags(
        vault.database_path(),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    connection.busy_timeout(std::time::Duration::from_secs(5))?;
    connection.execute_batch(
        "PRAGMA foreign_keys = ON;
         PRAGMA trusted_schema = OFF;",
    )?;
    Ok(connection)
}

fn create_schema(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS memory_meta (
             key TEXT PRIMARY KEY,
             value TEXT NOT NULL
         );
         INSERT INTO memory_meta(key, value)
             VALUES ('schema_version', '1')
             ON CONFLICT(key) DO NOTHING;
         CREATE TABLE IF NOT EXISTS nodes (
             id TEXT PRIMARY KEY,
             path TEXT NOT NULL UNIQUE,
             note_type TEXT NOT NULL,
             state TEXT NOT NULL,
             title TEXT,
             body TEXT NOT NULL,
             aliases TEXT NOT NULL DEFAULT '',
             tags TEXT NOT NULL DEFAULT '',
             metadata_json TEXT NOT NULL,
             content_hash BLOB NOT NULL,
             created_at TEXT NOT NULL,
             updated_at TEXT,
             modified_ns INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS edges (
             from_id TEXT NOT NULL,
             to_id TEXT NOT NULL,
             relation TEXT NOT NULL,
             reason TEXT,
             provenance TEXT NOT NULL,
             PRIMARY KEY (from_id, to_id, relation),
             FOREIGN KEY (from_id) REFERENCES nodes(id) ON DELETE CASCADE
         );
         CREATE INDEX IF NOT EXISTS edges_from ON edges(from_id);
         CREATE INDEX IF NOT EXISTS edges_to ON edges(to_id);
         CREATE INDEX IF NOT EXISTS nodes_type_state ON nodes(note_type, state);
         CREATE TABLE IF NOT EXISTS captures (
             event_key TEXT PRIMARY KEY,
             note_id TEXT NOT NULL UNIQUE,
             agent TEXT NOT NULL,
             event_kind TEXT NOT NULL,
             session_id TEXT,
             turn_id TEXT,
             payload_hash BLOB NOT NULL,
             captured_at TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS sources (
             note_id TEXT NOT NULL,
             ordinal INTEGER NOT NULL,
             source_json TEXT NOT NULL,
             PRIMARY KEY(note_id, ordinal),
             FOREIGN KEY(note_id) REFERENCES nodes(id) ON DELETE CASCADE
         );
         CREATE TABLE IF NOT EXISTS revisions (
             note_id TEXT NOT NULL,
             content_hash BLOB NOT NULL,
             recorded_at TEXT NOT NULL,
             markdown TEXT NOT NULL,
             PRIMARY KEY(note_id, content_hash)
         );
         CREATE TABLE IF NOT EXISTS edge_candidates (
             from_id TEXT NOT NULL,
             to_id TEXT NOT NULL,
             relation TEXT NOT NULL,
             explanation TEXT NOT NULL,
             evidence TEXT,
             generator TEXT NOT NULL,
             workflow_fingerprint TEXT NOT NULL,
             confidence REAL,
             status TEXT NOT NULL,
             PRIMARY KEY(from_id, to_id, relation, workflow_fingerprint)
         );
         CREATE TABLE IF NOT EXISTS promotion_runs (
             operation_key TEXT PRIMARY KEY,
             input_json TEXT NOT NULL,
             proposal_json TEXT NOT NULL,
             result_json TEXT,
             state TEXT NOT NULL,
             created_at TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS index_state (
             path TEXT PRIMARY KEY,
             size INTEGER NOT NULL,
             modified_ns INTEGER NOT NULL,
             content_hash BLOB NOT NULL
         );
         CREATE TABLE IF NOT EXISTS index_diagnostics (
             path TEXT PRIMARY KEY,
             class TEXT NOT NULL,
             message TEXT NOT NULL
         );
         CREATE VIRTUAL TABLE IF NOT EXISTS notes_fts USING fts5(
             id UNINDEXED,
             title,
             body,
             aliases,
             tags,
             tokenize = 'trigram'
         );",
    )?;
    Ok(())
}

fn clear_projection(connection: &mut Connection) -> Result<()> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    transaction.execute_batch(
        "DELETE FROM notes_fts;
         DELETE FROM captures;
         DELETE FROM sources;
         DELETE FROM edges;
         DELETE FROM revisions;
         DELETE FROM nodes;
         DELETE FROM index_state;
         DELETE FROM index_diagnostics;",
    )?;
    transaction.commit()?;
    Ok(())
}

#[derive(Debug)]
struct IndexState {
    size: u64,
    modified_ns: i64,
    hash: Vec<u8>,
}

fn load_index_state(connection: &Connection) -> Result<BTreeMap<String, IndexState>> {
    let mut statement = connection
        .prepare("SELECT path, size, modified_ns, content_hash FROM index_state ORDER BY path")?;
    let rows = statement.query_map([], |row| {
        let size = row.get::<_, i64>(1)?;
        Ok((
            row.get::<_, String>(0)?,
            IndexState {
                size: u64::try_from(size).unwrap_or_default(),
                modified_ns: row.get(2)?,
                hash: row.get(3)?,
            },
        ))
    })?;
    rows.collect::<std::result::Result<BTreeMap<_, _>, _>>()
        .map_err(MemoryError::from)
}

fn load_diagnostic_paths(connection: &Connection) -> Result<BTreeSet<String>> {
    let mut statement = connection.prepare("SELECT path FROM index_diagnostics ORDER BY path")?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    rows.collect::<std::result::Result<BTreeSet<_>, _>>()
        .map_err(MemoryError::from)
}

fn validate_changed_ids(
    connection: &Connection,
    changed: &[ChangedFile],
    removed: &HashSet<&str>,
) -> Result<()> {
    let mut batch = HashMap::<&str, &Path>::new();
    for file in changed {
        let ChangedFile::Valid(file) = file else {
            continue;
        };
        let id = file.note.frontmatter.id.as_str();
        if let Some(first) = batch.insert(id, &file.path) {
            return Err(MemoryError::Validation(format!(
                "duplicate note id `{id}` at {} and {}",
                first.display(),
                file.path.display()
            )));
        }
        let existing = connection
            .query_row("SELECT path FROM nodes WHERE id = ?1", [id], |row| {
                row.get::<_, String>(0)
            })
            .optional()?;
        if existing
            .is_some_and(|path| path != file.relative_path && !removed.contains(path.as_str()))
        {
            return Err(MemoryError::Validation(format!(
                "duplicate note id `{id}` conflicts with indexed path"
            )));
        }
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn upsert_file(
    transaction: &Transaction<'_>,
    file: &IndexedFile,
) -> Result<()> {
    let note = &file.note;
    let metadata_json = serde_json::to_string(&note.frontmatter)?;
    let aliases = note.frontmatter.aliases.join(" ");
    let tags = note.frontmatter.tags.join(" ");
    transaction.execute(
        "INSERT INTO nodes(
             id, path, note_type, state, title, body, aliases, tags, metadata_json,
             content_hash, created_at, updated_at, modified_ns
         ) VALUES (
             ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13
         )
         ON CONFLICT(id) DO UPDATE SET
             path = excluded.path,
             note_type = excluded.note_type,
             state = excluded.state,
             title = excluded.title,
             body = excluded.body,
             aliases = excluded.aliases,
             tags = excluded.tags,
             metadata_json = excluded.metadata_json,
             content_hash = excluded.content_hash,
             created_at = excluded.created_at,
             updated_at = excluded.updated_at,
             modified_ns = excluded.modified_ns",
        params![
            note.frontmatter.id,
            file.relative_path,
            note.frontmatter.note_type.to_string(),
            note.frontmatter.state.to_string(),
            note.frontmatter.title,
            note.body,
            aliases,
            tags,
            metadata_json,
            file.hash,
            note.frontmatter.created_at,
            note.frontmatter.updated_at,
            file.modified_ns,
        ],
    )?;
    transaction.execute(
        "DELETE FROM edges WHERE from_id = ?1",
        [&note.frontmatter.id],
    )?;
    for dependency in &note.frontmatter.deps {
        let Some(dependency) = dependency.local_id() else {
            continue;
        };
        transaction.execute(
            "INSERT INTO edges(from_id, to_id, relation, reason, provenance)
             VALUES (?1, ?2, 'depends_on', NULL, 'frontmatter:deps')",
            params![note.frontmatter.id, dependency],
        )?;
    }
    for link in &note.frontmatter.links {
        transaction.execute(
            "INSERT INTO edges(from_id, to_id, relation, reason, provenance)
             VALUES (?1, ?2, ?3, ?4, 'frontmatter:links')",
            params![
                note.frontmatter.id,
                link.to,
                link.rel.to_string(),
                link.reason
            ],
        )?;
    }
    for source_id in &note.frontmatter.promoted_from {
        transaction.execute(
            "INSERT INTO edges(from_id, to_id, relation, reason, provenance)
             VALUES (?1, ?2, 'source_of',
                     'Declared by promoted_from frontmatter',
                     'frontmatter:promoted_from')
             ON CONFLICT(from_id, to_id, relation) DO NOTHING",
            params![note.frontmatter.id, source_id],
        )?;
    }
    for superseded_id in &note.frontmatter.supersedes {
        transaction.execute(
            "INSERT INTO edges(from_id, to_id, relation, reason, provenance)
             VALUES (?1, ?2, 'supersedes',
                     'Declared by supersedes frontmatter',
                     'frontmatter:supersedes')
             ON CONFLICT(from_id, to_id, relation) DO NOTHING",
            params![note.frontmatter.id, superseded_id],
        )?;
    }
    transaction.execute(
        "DELETE FROM sources WHERE note_id = ?1",
        [&note.frontmatter.id],
    )?;
    for (ordinal, source) in note.frontmatter.sources.iter().enumerate() {
        transaction.execute(
            "INSERT INTO sources(note_id, ordinal, source_json) VALUES (?1, ?2, ?3)",
            params![
                note.frontmatter.id,
                i64::try_from(ordinal).unwrap_or(i64::MAX),
                serde_json::to_string(source)?
            ],
        )?;
    }
    transaction.execute(
        "DELETE FROM notes_fts WHERE id = ?1",
        [&note.frontmatter.id],
    )?;
    transaction.execute(
        "INSERT INTO notes_fts(id, title, body, aliases, tags)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            note.frontmatter.id,
            note.frontmatter.title,
            note.body,
            aliases,
            tags
        ],
    )?;
    transaction.execute(
        "INSERT INTO index_state(path, size, modified_ns, content_hash)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(path) DO UPDATE SET
             size = excluded.size,
             modified_ns = excluded.modified_ns,
             content_hash = excluded.content_hash",
        params![
            file.relative_path,
            i64::try_from(file.size).unwrap_or(i64::MAX),
            file.modified_ns,
            file.hash
        ],
    )?;
    transaction.execute(
        "DELETE FROM index_diagnostics WHERE path = ?1",
        [&file.relative_path],
    )?;
    if let Some(event_key) = yaml_string(&note.frontmatter.extra, "capture_event_key") {
        let agent = yaml_string(&note.frontmatter.extra, "capture_agent")
            .unwrap_or_else(|| "unknown".into());
        let event_kind = yaml_string(&note.frontmatter.extra, "capture_event_kind")
            .unwrap_or_else(|| "unknown".into());
        let session_id = yaml_string(&note.frontmatter.extra, "capture_session_id");
        let turn_id = yaml_string(&note.frontmatter.extra, "capture_turn_id");
        transaction.execute(
            "INSERT INTO captures(
                 event_key, note_id, agent, event_kind, session_id, turn_id,
                 payload_hash, captured_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(event_key) DO UPDATE SET note_id = excluded.note_id",
            params![
                event_key,
                note.frontmatter.id,
                agent,
                event_kind,
                session_id,
                turn_id,
                file.hash,
                note.frontmatter.created_at
            ],
        )?;
    }
    Ok(())
}

fn delete_by_path(
    transaction: &Transaction<'_>,
    path: &str,
) -> Result<()> {
    let id = transaction
        .query_row("SELECT id FROM nodes WHERE path = ?1", [path], |row| {
            row.get::<_, String>(0)
        })
        .optional()?;
    if let Some(id) = id {
        transaction.execute("DELETE FROM notes_fts WHERE id = ?1", [&id])?;
        transaction.execute("DELETE FROM nodes WHERE id = ?1", [&id])?;
    }
    transaction.execute("DELETE FROM index_state WHERE path = ?1", [path])?;
    transaction.execute("DELETE FROM index_diagnostics WHERE path = ?1", [path])?;
    Ok(())
}

fn replace_revisions(
    transaction: &Transaction<'_>,
    revisions: &[RevisionSnapshot],
) -> Result<()> {
    transaction.execute("DELETE FROM revisions", [])?;
    for revision in revisions {
        transaction.execute(
            "INSERT INTO revisions(note_id, content_hash, recorded_at, markdown)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                revision.note_id,
                revision.content_hash,
                revision.recorded_at,
                revision.markdown
            ],
        )?;
    }
    Ok(())
}

fn load_revision_snapshots(vault: &Vault) -> Result<Vec<RevisionSnapshot>> {
    let root = vault.root.join(".revisions");
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut snapshots = Vec::new();
    for entry in WalkBuilder::new(&root)
        .hidden(false)
        .follow_links(false)
        .build()
    {
        let entry = entry.map_err(|error| MemoryError::InvalidNote {
            path: root.clone(),
            message: error.to_string(),
        })?;
        if !entry.file_type().is_some_and(|kind| kind.is_file())
            || entry.path().extension().and_then(|value| value.to_str()) != Some("md")
        {
            continue;
        }
        let path = entry.path();
        let note_id = path
            .parent()
            .and_then(Path::file_name)
            .and_then(|value| value.to_str())
            .ok_or_else(|| MemoryError::InvalidNote {
                path: path.to_owned(),
                message: "revision has no note-id directory".into(),
            })?;
        crate::model::validate_id(note_id)?;
        let hash = path
            .file_stem()
            .and_then(|value| value.to_str())
            .ok_or_else(|| MemoryError::InvalidNote {
                path: path.to_owned(),
                message: "revision has no content hash".into(),
            })?;
        if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(MemoryError::InvalidNote {
                path: path.to_owned(),
                message: "revision filename must be a SHA-256 digest".into(),
            });
        }
        let markdown = fs::read_to_string(path).map_err(|error| MemoryError::io(path, error))?;
        if markdown.len() > crate::model::MAX_NOTE_BYTES {
            return Err(MemoryError::InputTooLarge {
                limit: crate::model::MAX_NOTE_BYTES,
            });
        }
        if sha256_hex(markdown.as_bytes()) != hash {
            return Err(MemoryError::InvalidNote {
                path: path.to_owned(),
                message: "revision content does not match its hash".into(),
            });
        }
        let metadata = fs::metadata(path).map_err(|error| MemoryError::io(path, error))?;
        snapshots.push(RevisionSnapshot {
            note_id: note_id.into(),
            content_hash: hash.as_bytes().to_vec(),
            recorded_at: modified_ns(&metadata).to_string(),
            markdown,
        });
    }
    snapshots.sort_by(|left, right| {
        (&left.note_id, &left.content_hash).cmp(&(&right.note_id, &right.content_hash))
    });
    Ok(snapshots)
}

fn upsert_diagnostic(
    transaction: &Transaction<'_>,
    relative_path: &str,
    diagnostic: &Diagnostic,
) -> Result<()> {
    transaction.execute(
        "INSERT INTO index_diagnostics(path, class, message)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(path) DO UPDATE SET
             class = excluded.class,
             message = excluded.message",
        params![relative_path, diagnostic.class, diagnostic.message],
    )?;
    Ok(())
}

fn search_fts(
    connection: &Connection,
    query: &str,
    limit: usize,
) -> Result<Vec<SearchHit>> {
    match search_fts_once(connection, query, limit) {
        Ok(hits) => Ok(hits),
        Err(error) if is_fts_query_error(&error) => {
            match search_fts_once(connection, &quote_fts(query), limit) {
                Ok(hits) => Ok(hits),
                Err(error) if is_fts_query_error(&error) => Ok(Vec::new()),
                Err(error) => Err(MemoryError::Sqlite(error)),
            }
        },
        Err(error) => Err(MemoryError::Sqlite(error)),
    }
}

fn search_fts_once(
    connection: &Connection,
    query: &str,
    limit: usize,
) -> std::result::Result<Vec<SearchHit>, rusqlite::Error> {
    let mut statement = connection.prepare(
        "SELECT nodes.id, nodes.note_type, nodes.state, nodes.title,
                snippet(notes_fts, 2, '[', ']', ' … ', 18),
                -bm25(notes_fts, 8.0, 4.0, 1.5, 2.0) +
                    CASE WHEN nodes.id = ?1 THEN 100.0 ELSE 0.0 END +
                    CASE WHEN nodes.title = ?1 THEN 25.0 ELSE 0.0 END +
                    CASE WHEN instr(' ' || lower(nodes.tags) || ' ',
                                    ' ' || lower(?1) || ' ') > 0
                         THEN 20.0 ELSE 0.0 END +
                    CASE WHEN EXISTS (
                        SELECT 1 FROM edges
                        WHERE (from_id = nodes.id AND to_id = ?1)
                           OR (to_id = nodes.id AND from_id = ?1)
                    ) THEN 8.0 ELSE 0.0 END +
                    CASE WHEN EXISTS (
                        SELECT 1 FROM sources
                        WHERE sources.note_id = nodes.id
                          AND (source_json LIKE '%literature%'
                               OR source_json LIKE '%url%')
                    ) THEN 3.0 ELSE 0.0 END -
                    CASE WHEN nodes.state = 'archived' THEN 10.0 ELSE 0.0 END
         FROM notes_fts
         JOIN nodes ON nodes.id = notes_fts.id
         WHERE notes_fts MATCH ?1
         ORDER BY 6 DESC, nodes.id
         LIMIT ?2",
    )?;
    statement
        .query_map(
            params![query, i64::try_from(limit).unwrap_or(i64::MAX)],
            |row| {
                Ok(SearchHit {
                    id: row.get(0)?,
                    note_type: row.get(1)?,
                    state: row.get(2)?,
                    title: row.get(3)?,
                    snippet: row.get(4)?,
                    score: row.get(5)?,
                })
            },
        )
        .and_then(|rows| rows.collect::<std::result::Result<Vec<_>, _>>())
}

fn is_fts_query_error(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(_, Some(message))
            if message.contains("fts5")
                || message.contains("syntax error")
                || message.contains("unterminated")
                || message.contains("no such column")
    )
}

fn search_substring(
    connection: &Connection,
    query: &str,
    limit: usize,
) -> Result<Vec<SearchHit>> {
    let pattern = format!("%{}%", escape_like(query));
    let mut statement = connection.prepare(
        "SELECT id, note_type, state, title,
                CASE
                    WHEN instr(body, ?1) > 0
                    THEN substr(body, max(1, instr(body, ?1) - 30), 120)
                    ELSE coalesce(title, id)
                END,
                CASE
                    WHEN id = ?1 THEN 100.0
                    WHEN title = ?1 THEN 25.0
                    ELSE 1.0
                END +
                CASE WHEN instr(' ' || lower(tags) || ' ',
                                ' ' || lower(?1) || ' ') > 0
                     THEN 20.0 ELSE 0.0 END +
                CASE WHEN EXISTS (
                    SELECT 1 FROM edges
                    WHERE (from_id = nodes.id AND to_id = ?1)
                       OR (to_id = nodes.id AND from_id = ?1)
                ) THEN 8.0 ELSE 0.0 END +
                CASE WHEN EXISTS (
                    SELECT 1 FROM sources
                    WHERE sources.note_id = nodes.id
                      AND (source_json LIKE '%literature%'
                           OR source_json LIKE '%url%')
                ) THEN 3.0 ELSE 0.0 END -
                CASE WHEN state = 'archived' THEN 10.0 ELSE 0.0 END
         FROM nodes
         WHERE id LIKE ?2 ESCAPE '\\'
            OR title LIKE ?2 ESCAPE '\\'
            OR body LIKE ?2 ESCAPE '\\'
            OR aliases LIKE ?2 ESCAPE '\\'
            OR tags LIKE ?2 ESCAPE '\\'
         ORDER BY 6 DESC, id
         LIMIT ?3",
    )?;
    let rows = statement.query_map(
        params![query, pattern, i64::try_from(limit).unwrap_or(i64::MAX)],
        |row| {
            Ok(SearchHit {
                id: row.get(0)?,
                note_type: row.get(1)?,
                state: row.get(2)?,
                title: row.get(3)?,
                snippet: row.get(4)?,
                score: row.get(5)?,
            })
        },
    )?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(MemoryError::from)
}

fn edges(
    vault: &Vault,
    column: &str,
    id: &str,
) -> Result<Vec<Edge>> {
    ensure_note_exists(vault, id)?;
    let connection = open_read(vault)?;
    let sql = format!(
        "SELECT from_id, to_id, relation, reason
         FROM edges WHERE {column} = ?1 ORDER BY from_id, to_id, relation"
    );
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map([id], row_to_edge)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(MemoryError::from)
}

fn all_knowledge_edges(connection: &Connection) -> Result<Vec<Edge>> {
    let mut statement = connection.prepare(
        "SELECT edges.from_id, edges.to_id, edges.relation, edges.reason
         FROM edges
         JOIN nodes AS source ON source.id = edges.from_id
         JOIN nodes AS target ON target.id = edges.to_id
         WHERE edges.relation != 'depends_on'
         ORDER BY from_id, to_id, relation",
    )?;
    let rows = statement.query_map([], row_to_edge)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(MemoryError::from)
}

fn dependency_edges(connection: &Connection) -> Result<Vec<Edge>> {
    let mut statement = connection.prepare(
        "SELECT edges.from_id, edges.to_id, edges.relation, edges.reason
         FROM edges
         JOIN nodes AS source ON source.id = edges.from_id
         JOIN nodes AS target ON target.id = edges.to_id
         WHERE edges.relation = 'depends_on'
         ORDER BY from_id, to_id",
    )?;
    let rows = statement.query_map([], row_to_edge)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(MemoryError::from)
}

fn row_to_edge(row: &rusqlite::Row<'_>) -> rusqlite::Result<Edge> {
    Ok(Edge {
        from: row.get(0)?,
        to: row.get(1)?,
        relation: row.get(2)?,
        reason: row.get(3)?,
    })
}

fn ensure_note_exists(
    vault: &Vault,
    id: &str,
) -> Result<()> {
    let connection = open_read(vault)?;
    let exists = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM nodes WHERE id = ?1)",
        [id],
        |row| row.get::<_, bool>(0),
    )?;
    if exists {
        Ok(())
    } else {
        Err(MemoryError::NoteNotFound(id.into()))
    }
}

fn append_graph_diagnostics(
    notes: &[(PathBuf, Note)],
    diagnostics: &mut Vec<DoctorDiagnostic>,
) {
    let ids = notes
        .iter()
        .map(|(_, note)| note.frontmatter.id.as_str())
        .collect::<HashSet<_>>();
    let dependencies = notes
        .iter()
        .map(|(_, note)| {
            (
                note.frontmatter.id.as_str(),
                note.frontmatter
                    .deps
                    .iter()
                    .filter_map(crate::model::Dependency::local_id)
                    .collect::<Vec<_>>(),
            )
        })
        .collect::<HashMap<_, _>>();
    for (path, note) in notes {
        for dependency in &note.frontmatter.deps {
            let Some(dependency) = dependency.local_id() else {
                continue;
            };
            if !ids.contains(dependency) {
                diagnostics.push(DoctorDiagnostic {
                    class: "unresolved_dependency".into(),
                    severity: "error".into(),
                    note_id: Some(note.frontmatter.id.clone()),
                    path: Some(path.clone()),
                    message: format!("dependency `{dependency}` does not resolve"),
                });
            }
        }
        for link in &note.frontmatter.links {
            if !ids.contains(link.to.as_str()) {
                diagnostics.push(DoctorDiagnostic {
                    class: "dangling_link".into(),
                    severity: "warning".into(),
                    note_id: Some(note.frontmatter.id.clone()),
                    path: Some(path.clone()),
                    message: format!("{} target `{}` does not resolve", link.rel, link.to),
                });
            }
        }
    }
    let mut colors = HashMap::<&str, u8>::new();
    let mut stack = Vec::<&str>::new();
    for id in dependencies.keys() {
        detect_cycle(id, &dependencies, &mut colors, &mut stack, diagnostics);
    }
}

fn detect_cycle<'a>(
    id: &'a str,
    graph: &HashMap<&'a str, Vec<&'a str>>,
    colors: &mut HashMap<&'a str, u8>,
    stack: &mut Vec<&'a str>,
    diagnostics: &mut Vec<DoctorDiagnostic>,
) {
    match colors.get(id).copied() {
        Some(2) => return,
        Some(1) => {
            if let Some(position) = stack.iter().position(|entry| *entry == id) {
                let mut cycle = stack[position..].to_vec();
                cycle.push(id);
                diagnostics.push(DoctorDiagnostic {
                    class: "dependency_cycle".into(),
                    severity: "error".into(),
                    note_id: Some(id.into()),
                    path: None,
                    message: format!("dependency cycle: {}", cycle.join(" -> ")),
                });
            }
            return;
        },
        _ => {},
    }
    colors.insert(id, 1);
    stack.push(id);
    if let Some(neighbors) = graph.get(id) {
        for neighbor in neighbors {
            if graph.contains_key(neighbor) {
                detect_cycle(neighbor, graph, colors, stack, diagnostics);
            }
        }
    }
    stack.pop();
    colors.insert(id, 2);
}

fn append_index_diagnostics(
    vault: &Vault,
    connection: &Connection,
    notes: &[(PathBuf, Note)],
    diagnostics: &mut Vec<DoctorDiagnostic>,
) -> Result<()> {
    let indexed = load_index_state(connection)?;
    let mut disk = BTreeSet::new();
    for (path, _) in notes {
        let relative = relative_path(vault, path)?;
        disk.insert(relative.clone());
        let metadata = fs::metadata(path).map_err(|error| MemoryError::io(path, error))?;
        let hash = fs::read(path)
            .map(|bytes| Sha256::digest(bytes).to_vec())
            .map_err(|error| MemoryError::io(path, error))?;
        let stale = indexed.get(&relative).is_none_or(|state| {
            state.size != metadata.len()
                || state.modified_ns != modified_ns(&metadata)
                || state.hash != hash
        });
        if stale {
            diagnostics.push(DoctorDiagnostic {
                class: "unindexed_note".into(),
                severity: "warning".into(),
                note_id: None,
                path: Some(path.clone()),
                message: "note is missing from the index or has changed".into(),
            });
        }
    }
    for path in indexed.keys().filter(|path| !disk.contains(*path)) {
        diagnostics.push(DoctorDiagnostic {
            class: "stale_index_row".into(),
            severity: "warning".into(),
            note_id: None,
            path: Some(vault.root.join(path)),
            message: "index row has no corresponding Markdown file".into(),
        });
    }
    let mut statement =
        connection.prepare("SELECT path, class, message FROM index_diagnostics ORDER BY path")?;
    let rows = statement.query_map([], |row| {
        Ok(DoctorDiagnostic {
            class: row.get(1)?,
            severity: "error".into(),
            note_id: None,
            path: Some(vault.root.join(row.get::<_, String>(0)?)),
            message: row.get(2)?,
        })
    })?;
    diagnostics.extend(rows.collect::<std::result::Result<Vec<_>, _>>()?);
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn append_operational_diagnostics(
    vault: &Vault,
    connection: &Connection,
    notes: &[(PathBuf, Note)],
    diagnostics: &mut Vec<DoctorDiagnostic>,
) -> Result<()> {
    let connected = all_knowledge_edges(connection)?
        .into_iter()
        .chain(dependency_edges(connection)?)
        .flat_map(|edge| [edge.from, edge.to])
        .collect::<HashSet<_>>();
    for (path, note) in notes {
        if note.frontmatter.state != NoteState::Archived
            && !connected.contains(&note.frontmatter.id)
        {
            diagnostics.push(DoctorDiagnostic {
                class: if note.frontmatter.note_type == NoteType::Permanent {
                    "unreachable_permanent"
                } else {
                    "orphan_note"
                }
                .into(),
                severity: "warning".into(),
                note_id: Some(note.frontmatter.id.clone()),
                path: Some(path.clone()),
                message: "note has no resolved incoming or outgoing graph edge".into(),
            });
        }
    }

    let mut promotion_statement = connection.prepare(
        "SELECT operation_key, input_json
         FROM promotion_runs WHERE state = 'proposed' ORDER BY operation_key",
    )?;
    let promotion_rows = promotion_statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut pending_promotions = 0_usize;
    for row in promotion_rows {
        let (operation_key, input_json) = row?;
        pending_promotions += 1;
        let input: serde_json::Value = serde_json::from_str(&input_json)?;
        let stale = input
            .get("source_revisions")
            .and_then(serde_json::Value::as_object)
            .is_some_and(|revisions| {
                revisions.iter().any(|(id, expected)| {
                    let Some(expected) = expected.as_str() else {
                        return true;
                    };
                    let path = connection
                        .query_row("SELECT path FROM nodes WHERE id = ?1", [id], |row| {
                            row.get::<_, String>(0)
                        })
                        .optional()
                        .ok()
                        .flatten()
                        .map(|path| vault.root.join(path));
                    path.and_then(|path| fs::read(path).ok())
                        .is_none_or(|markdown| sha256_hex(&markdown) != expected)
                })
            });
        if stale {
            diagnostics.push(DoctorDiagnostic {
                class: "stale_promotion_proposal".into(),
                severity: "warning".into(),
                note_id: None,
                path: Some(vault.database_path()),
                message: format!(
                    "promotion proposal `{operation_key}` references a changed source revision"
                ),
            });
        }
    }
    if pending_promotions > 0
        && !diagnostics
            .iter()
            .any(|diagnostic| diagnostic.class == "stale_promotion_proposal")
    {
        diagnostics.push(DoctorDiagnostic {
            class: "pending_promotion_proposal".into(),
            severity: "warning".into(),
            note_id: None,
            path: Some(vault.database_path()),
            message: format!("{pending_promotions} promotion proposal(s) remain unapplied"),
        });
    }

    if let Err(error) = vault.lock_writer(false) {
        diagnostics.push(DoctorDiagnostic {
            class: "lock_health".into(),
            severity: if matches!(error, MemoryError::WriterBusy) {
                "warning"
            } else {
                "error"
            }
            .into(),
            note_id: None,
            path: Some(vault.index_dir().join("writer.lock")),
            message: error.to_string(),
        });
    }

    let transaction_root = vault.index_dir().join("transactions");
    if transaction_root.exists() {
        for entry in fs::read_dir(&transaction_root)
            .map_err(|error| MemoryError::io(&transaction_root, error))?
        {
            let entry = entry.map_err(|error| MemoryError::io(&transaction_root, error))?;
            if entry
                .file_type()
                .map_err(|error| MemoryError::io(entry.path(), error))?
                .is_dir()
            {
                diagnostics.push(DoctorDiagnostic {
                    class: "incomplete_recovery".into(),
                    severity: "error".into(),
                    note_id: None,
                    path: Some(entry.path()),
                    message: "an incomplete filesystem transaction requires recovery".into(),
                });
            }
        }
    }
    if let Err(error) = load_revision_snapshots(vault) {
        diagnostics.push(DoctorDiagnostic {
            class: "corrupt_revision".into(),
            severity: "error".into(),
            note_id: None,
            path: Some(vault.root.join(".revisions")),
            message: error.to_string(),
        });
    }
    for entry in WalkBuilder::new(&vault.root)
        .hidden(false)
        .follow_links(false)
        .build()
        .filter_map(std::result::Result::ok)
    {
        if entry.file_type().is_some_and(|kind| kind.is_file())
            && entry.file_name().to_str().is_some_and(|name| {
                name.starts_with('.')
                    && Path::new(name)
                        .extension()
                        .is_some_and(|extension| extension.eq_ignore_ascii_case("tmp"))
            })
        {
            diagnostics.push(DoctorDiagnostic {
                class: "abandoned_temp_file".into(),
                severity: "warning".into(),
                note_id: None,
                path: Some(entry.into_path()),
                message: "abandoned atomic-write temporary file".into(),
            });
        }
    }

    if database_filesystem_is_unsupported(&vault.index_dir()) {
        diagnostics.push(DoctorDiagnostic {
            class: "database_locality".into(),
            severity: "error".into(),
            note_id: None,
            path: Some(vault.database_path()),
            message: "the active WAL database is on a network or synchronized filesystem".into(),
        });
    }
    let hook_status = crate::hook::status_codex_user();
    match hook_status {
        Ok(status) if !status.installed => diagnostics.push(DoctorDiagnostic {
            class: "hook_status".into(),
            severity: "warning".into(),
            note_id: None,
            path: Some(status.config_path),
            message: "the owned Codex Stop hook is not installed".into(),
        }),
        Err(error) => diagnostics.push(DoctorDiagnostic {
            class: "hook_status".into(),
            severity: "warning".into(),
            note_id: None,
            path: None,
            message: error.to_string(),
        }),
        Ok(_) => {},
    }
    Ok(())
}

fn sha256_hex(input: &[u8]) -> String {
    const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";

    let digest = Sha256::digest(input);
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        encoded.push(char::from(HEX_DIGITS[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX_DIGITS[usize::from(byte & 0x0f)]));
    }
    encoded
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn filesystem_type(path: &Path) -> Option<String> {
    std::process::Command::new("df")
        .arg("-T")
        .arg(path)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .and_then(|output| {
            output
                .lines()
                .last()
                .and_then(|line| line.split_whitespace().nth(1))
                .map(str::to_ascii_lowercase)
        })
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn filesystem_type(_path: &Path) -> Option<String> {
    None
}

fn database_filesystem_is_unsupported(path: &Path) -> bool {
    filesystem_type(path).is_some_and(|filesystem| {
        [
            "nfs", "smb", "cifs", "afp", "webdav", "sshfs", "9p", "davfs",
        ]
        .iter()
        .any(|network| filesystem.contains(network))
    })
}

fn ensure_local_database_filesystem(vault: &Vault) -> Result<()> {
    if database_filesystem_is_unsupported(&vault.index_dir()) {
        return Err(MemoryError::InvalidConfig(format!(
            "active SQLite WAL storage must be local: {} is on an unsupported filesystem",
            vault.index_dir().display()
        )));
    }
    Ok(())
}

fn note_paths(vault: &Vault) -> Result<Vec<PathBuf>> {
    let root = vault.root.clone();
    let walker = WalkBuilder::new(&root)
        .hidden(false)
        .follow_links(false)
        .filter_entry(move |entry| include_entry(entry, &root))
        .build();
    let mut paths = Vec::new();
    for entry in walker {
        let entry = entry.map_err(|error| MemoryError::InvalidNote {
            path: vault.root.clone(),
            message: error.to_string(),
        })?;
        if entry
            .file_type()
            .is_some_and(|file_type| file_type.is_file() || file_type.is_symlink())
            && entry.path().extension().and_then(|value| value.to_str()) == Some("md")
        {
            let parent = entry
                .path()
                .parent()
                .and_then(Path::file_name)
                .and_then(|value| value.to_str());
            if parent.is_some_and(is_note_directory) {
                paths.push(entry.into_path());
            }
        }
    }
    paths.sort();
    Ok(paths)
}

fn include_entry(
    entry: &DirEntry,
    root: &Path,
) -> bool {
    if entry.path() == root {
        return true;
    }
    entry.file_name() != ".index"
}

fn is_note_directory(directory: &str) -> bool {
    matches!(
        directory,
        "fleeting" | "literature" | "permanent" | "structure" | "index" | "archived"
    )
}

fn relative_path(
    vault: &Vault,
    path: &Path,
) -> Result<String> {
    path.strip_prefix(&vault.root)
        .map_err(|_| MemoryError::UnsafeInput("note path escapes registered vault".into()))?
        .to_str()
        .map(str::to_owned)
        .ok_or_else(|| MemoryError::UnsafeInput("note path is not valid UTF-8".into()))
}

fn ensure_inside_vault(
    vault: &Vault,
    path: &Path,
) -> Result<()> {
    let canonical = fs::canonicalize(path).map_err(|error| MemoryError::io(path, error))?;
    if canonical.starts_with(&vault.root) {
        Ok(())
    } else {
        Err(MemoryError::UnsafeInput(
            "indexed note path escapes registered vault".into(),
        ))
    }
}

fn modified_ns(metadata: &fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |duration| {
            i64::try_from(duration.as_nanos()).unwrap_or(i64::MAX)
        })
}

fn yaml_string(
    fields: &BTreeMap<String, yaml_serde::Value>,
    key: &str,
) -> Option<String> {
    match fields.get(key) {
        Some(yaml_serde::Value::String(value)) => Some(value.clone()),
        _ => None,
    }
}

fn quote_fts(query: &str) -> String {
    format!("\"{}\"", query.replace('"', "\"\""))
}

fn escape_like(query: &str) -> String {
    query
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

fn write_diagnostics(
    vault: &Vault,
    diagnostics: &[Diagnostic],
) -> Result<()> {
    let path = vault.index_dir().join("diagnostics").join("index.json");
    let bytes = serde_json::to_vec_pretty(diagnostics)?;
    crate::fsutil::write_atomic_replace(&path, &bytes)
}

fn sqlite_version_at_least(
    version: &str,
    required: (u32, u32, u32),
) -> bool {
    let mut parts = version
        .split('.')
        .take(3)
        .map(|part| part.parse::<u32>().unwrap_or_default());
    let parsed = (
        parts.next().unwrap_or_default(),
        parts.next().unwrap_or_default(),
        parts.next().unwrap_or_default(),
    );
    parsed >= required
}

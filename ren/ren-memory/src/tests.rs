use std::{
    fs,
    io::Write as _,
    path::Path,
    sync::{Arc, Barrier},
};

use serde_json::json;
use tempfile::TempDir;
use ulid::Ulid;
use yaml_serde::Value as YamlValue;

use crate::{
    MemoryHome,
    capture::{CaptureEvent, EVENT_SCHEMA, capture_event, parse_event},
    fsutil::publish_new,
    hook, index,
    model::{Dependency, Link, Note, NoteState, NoteType, Relation, SCHEMA, Source},
    mutation,
    vault::Vault,
};

#[derive(Debug, PartialEq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenAiMetadata {
    interface: OpenAiInterface,
}

#[derive(Debug, PartialEq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenAiInterface {
    display_name: String,
    short_description: String,
    default_prompt: String,
}

fn fixture() -> crate::error::Result<(TempDir, MemoryHome, Vault)> {
    let temporary = tempfile::tempdir().map_err(|error| crate::MemoryError::io(".", error))?;
    let project = temporary.path().join("project");
    fs::create_dir(&project).map_err(|error| crate::MemoryError::io(&project, error))?;
    let home = MemoryHome {
        root: temporary.path().join("memory"),
    };
    let vault = home.register(Some("test-vault"), None, &project)?;
    Ok((temporary, home, vault))
}

fn write_note(
    vault: &Vault,
    note: &Note,
) -> crate::error::Result<std::path::PathBuf> {
    let path =
        vault.safe_note_path(note.frontmatter.note_type.directory(), &note.frontmatter.id)?;
    publish_new(&path, note.to_markdown()?.as_bytes())?;
    Ok(path)
}

fn note(
    note_type: NoteType,
    state: NoteState,
    title: &str,
    body: &str,
) -> Note {
    Note::new(
        note_type,
        state,
        "2026-07-29T10:00:00Z".into(),
        Some(title.into()),
        body.into(),
    )
}

#[test]
fn note_round_trip_preserves_unknown_frontmatter() -> crate::error::Result<()> {
    let id = Ulid::generate().to_string();
    let path = Path::new("fleeting").join(format!("{id}.md"));
    let input = format!(
        "---\nschema: {SCHEMA}\nid: {id}\ntype: fleeting\nstate: inbox\ncreated_at: \
         2026-07-29T10:00:00Z\nobsidian-cssclasses:\n  - wide\n---\n\nA portable note.\n"
    );
    let parsed = Note::parse(&path, &input)?;
    assert_eq!(
        parsed.frontmatter.extra["obsidian-cssclasses"],
        YamlValue::Sequence(vec![YamlValue::String("wide".into())])
    );

    let encoded = parsed.to_markdown()?;
    let reparsed = Note::parse(&path, &encoded)?;
    assert_eq!(parsed, reparsed);
    Ok(())
}

#[test]
fn accepted_links_require_reasons() {
    let mut source = note(
        NoteType::Permanent,
        NoteState::Accepted,
        "Source",
        "Source body",
    );
    source.frontmatter.links.push(Link {
        to: Ulid::generate().to_string(),
        rel: Relation::Refines,
        reason: None,
    });
    let error = source.frontmatter.validate();
    assert!(
        error.is_err_and(|error| error.to_string().contains("requires a reason")),
        "accepted link without a reason was not rejected"
    );
}

#[test]
fn registry_and_layout_are_user_scoped_and_resolvable() -> crate::error::Result<()> {
    let (_temporary, home, vault) = fixture()?;
    assert!(home.root.join("registry.json").is_file());
    assert!(home.root.join("config.toml").is_file());
    for directory in [
        "fleeting",
        "literature",
        "permanent",
        "structure",
        "index",
        "archived",
        ".index/diagnostics",
    ] {
        assert!(vault.root.join(directory).is_dir(), "missing {directory}");
    }
    let resolved = home.resolve(None, &vault.project_path)?;
    assert_eq!(resolved.id, vault.id);
    assert_eq!(resolved.root, vault.root);
    Ok(())
}

#[test]
fn linked_git_worktree_resolves_the_primary_worktree_vault() -> crate::error::Result<()> {
    let temporary = tempfile::tempdir().map_err(|error| crate::MemoryError::io(".", error))?;
    let primary = temporary.path().join("repository");
    let common_dir = primary.join(".git");
    fs::create_dir_all(&common_dir).map_err(|error| crate::MemoryError::io(&common_dir, error))?;
    let linked = temporary.path().join("repository-worktree");
    fs::create_dir(&linked).map_err(|error| crate::MemoryError::io(&linked, error))?;
    let linked_git_dir = common_dir.join("worktrees/repository-worktree");
    fs::create_dir_all(&linked_git_dir)
        .map_err(|error| crate::MemoryError::io(&linked_git_dir, error))?;
    fs::write(
        linked.join(".git"),
        format!("gitdir: {}\n", linked_git_dir.display()),
    )
    .map_err(|error| crate::MemoryError::io(linked.join(".git"), error))?;
    fs::write(linked_git_dir.join("commondir"), "../..\n")
        .map_err(|error| crate::MemoryError::io(linked_git_dir.join("commondir"), error))?;

    let home = MemoryHome {
        root: temporary.path().join("memory"),
    };
    let primary_vault = home.register(Some("primary"), None, &primary)?;
    let other_linked = temporary.path().join("other-worktree");
    fs::create_dir(&other_linked).map_err(|error| crate::MemoryError::io(&other_linked, error))?;
    let other_git_dir = common_dir.join("worktrees/other-worktree");
    fs::create_dir_all(&other_git_dir)
        .map_err(|error| crate::MemoryError::io(&other_git_dir, error))?;
    fs::write(
        other_linked.join(".git"),
        format!("gitdir: {}\n", other_git_dir.display()),
    )
    .map_err(|error| crate::MemoryError::io(other_linked.join(".git"), error))?;
    fs::write(other_git_dir.join("commondir"), "../..\n")
        .map_err(|error| crate::MemoryError::io(other_git_dir.join("commondir"), error))?;
    home.register(Some("other-linked"), None, &other_linked)?;

    assert_eq!(home.resolve_or_register_hint(&linked)?.id, primary_vault.id);
    assert_eq!(home.resolve(None, &linked)?.id, primary_vault.id);
    Ok(())
}

#[test]
fn incremental_index_supports_search_and_graph_queries() -> crate::error::Result<()> {
    let (_temporary, _home, vault) = fixture()?;
    let target = note(
        NoteType::Permanent,
        NoteState::Accepted,
        "SQLite derived index",
        "SQLite is a disposable projection over Markdown.",
    );
    let target_id = target.frontmatter.id.clone();
    write_note(&vault, &target)?;

    let mut source = note(
        NoteType::Permanent,
        NoteState::Accepted,
        "Capture before indexing",
        "The durable capture boundary is the Markdown file.",
    );
    let source_id = source.frontmatter.id.clone();
    source
        .frontmatter
        .deps
        .push(Dependency::Local(target_id.clone()));
    source.frontmatter.links.push(Link {
        to: target_id.clone(),
        rel: Relation::Refines,
        reason: Some("Adds the concurrency boundary.".into()),
    });
    write_note(&vault, &source)?;

    let first = index::sync(&vault, false, true)?;
    assert_eq!(first.indexed, 2);
    assert!(first.invalid.is_empty());
    let second = index::sync(&vault, false, true)?;
    assert_eq!(second.unchanged, 2);
    assert_eq!(second.indexed, 0);

    let hits = index::search(&vault, "disposable projection", 20)?;
    assert_eq!(
        hits.first().map(|hit| hit.id.as_str()),
        Some(target_id.as_str())
    );
    let deps = index::edges_from(&vault, &source_id)?;
    assert!(
        deps.iter()
            .any(|edge| { edge.to == target_id && edge.relation == "depends_on" })
    );
    assert!(
        deps.iter()
            .any(|edge| { edge.to == target_id && edge.relation == "refines" })
    );
    let refs = index::edges_to(&vault, &target_id)?;
    assert_eq!(refs.len(), 2);
    assert_eq!(
        index::shortest_path(&vault, &source_id, &target_id)?,
        [source_id, target_id]
    );
    Ok(())
}

#[test]
fn rebuild_and_deletion_are_deterministic() -> crate::error::Result<()> {
    let (_temporary, _home, vault) = fixture()?;
    let first = note(
        NoteType::Fleeting,
        NoteState::Inbox,
        "First",
        "First note body",
    );
    let first_path = write_note(&vault, &first)?;
    let second = note(
        NoteType::Permanent,
        NoteState::Accepted,
        "Second",
        "Second note body",
    );
    write_note(&vault, &second)?;
    index::sync(&vault, false, true)?;
    let before = serde_json::to_value(index::all_notes(&vault)?)?;
    index::sync(&vault, true, true)?;
    let after = serde_json::to_value(index::all_notes(&vault)?)?;
    assert_eq!(before, after);

    fs::remove_file(&first_path).map_err(|error| crate::MemoryError::io(&first_path, error))?;
    let report = index::sync(&vault, false, true)?;
    assert_eq!(report.removed, 1);
    assert!(matches!(
        index::note_path(&vault, &first.frontmatter.id),
        Err(crate::MemoryError::NoteNotFound(_))
    ));
    Ok(())
}

#[test]
fn capture_is_idempotent_and_redacts_known_secrets() -> crate::error::Result<()> {
    let (_temporary, _home, vault) = fixture()?;
    let event = CaptureEvent {
        schema: EVENT_SCHEMA.into(),
        agent: "codex".into(),
        event_kind: "stop".into(),
        session_id: Some("session-1".into()),
        turn_id: Some("turn-1".into()),
        vault_hint: vault.project_path.to_string_lossy().into_owned(),
        occurred_at: "2026-07-29T10:00:00Z".into(),
        title: None,
        content: "Use local storage.\napi_key=do-not-store-this".into(),
    };
    let first = capture_event(&vault, &event)?;
    let second = capture_event(&vault, &event)?;
    assert!(first.captured);
    assert!(!second.captured);
    assert_eq!(first.note_id, second.note_id);

    let markdown = fs::read_to_string(&first.path)
        .map_err(|error| crate::MemoryError::io(&first.path, error))?;
    assert!(!markdown.contains("do-not-store-this"));
    assert!(markdown.contains("[REDACTED]"));
    assert_eq!(index::all_notes(&vault)?.len(), 1);
    Ok(())
}

#[test]
fn private_keys_are_rejected_before_capture() -> crate::error::Result<()> {
    let (_temporary, _home, vault) = fixture()?;
    let event = CaptureEvent {
        schema: EVENT_SCHEMA.into(),
        agent: "manual".into(),
        event_kind: "capture".into(),
        session_id: None,
        turn_id: None,
        vault_hint: vault.project_path.to_string_lossy().into_owned(),
        occurred_at: "2026-07-29T10:00:00Z".into(),
        title: None,
        content: "-----BEGIN PRIVATE KEY-----\nsecret".into(),
    };
    assert!(matches!(
        capture_event(&vault, &event),
        Err(crate::MemoryError::UnsafeInput(_))
    ));
    assert!(
        fs::read_dir(vault.root.join("fleeting"))
            .map_err(|error| crate::MemoryError::io(vault.root.join("fleeting"), error))?
            .next()
            .is_none()
    );
    Ok(())
}

#[test]
fn codex_stop_payload_is_normalized_without_transcript_capture() -> crate::error::Result<()> {
    let (_temporary, _home, vault) = fixture()?;
    let raw = serde_json::to_vec(&json!({
        "session_id": "session",
        "turn_id": "turn",
        "transcript_path": "/private/transcript.jsonl",
        "cwd": vault.project_path,
        "hook_event_name": "Stop",
        "last_assistant_message": "Implemented the requested change.",
        "stop_hook_active": false
    }))?;
    let first_event = parse_event(&raw, "codex", "stop")?;
    let second_event = parse_event(&raw, "codex", "stop")?;
    assert_eq!(first_event.session_id.as_deref(), Some("session"));
    assert_eq!(first_event.turn_id.as_deref(), Some("turn"));
    assert_eq!(first_event.content, "Implemented the requested change.");
    let encoded = serde_json::to_string(&first_event)?;
    assert!(!encoded.contains("transcript"));
    let first = capture_event(&vault, &first_event)?;
    let second = capture_event(&vault, &second_event)?;
    assert!(first.captured);
    assert!(!second.captured);
    assert_eq!(first.note_id, second.note_id);
    Ok(())
}

#[test]
fn non_codex_adapter_validation_uses_component_name() {
    let error = parse_event(br#"{"content":"not normalized"}"#, "manual", "capture")
        .expect_err("non-Codex adapter input without a schema must be rejected");
    assert!(matches!(
        &error,
        crate::MemoryError::Validation(message)
            if message == "adapter payload is not a normalized ren-memory event"
    ));
    assert_eq!(
        error.to_string(),
        "validation failed: adapter payload is not a normalized ren-memory event"
    );
}

#[test]
fn doctor_reports_cycles_unresolved_dependencies_and_dangling_links() -> crate::error::Result<()> {
    let (_temporary, _home, vault) = fixture()?;
    let mut first = note(NoteType::Permanent, NoteState::Accepted, "First", "First");
    let mut second = note(NoteType::Permanent, NoteState::Accepted, "Second", "Second");
    first
        .frontmatter
        .deps
        .push(Dependency::Local(second.frontmatter.id.clone()));
    second
        .frontmatter
        .deps
        .push(Dependency::Local(first.frontmatter.id.clone()));
    second
        .frontmatter
        .deps
        .push(Dependency::Local(Ulid::generate().to_string()));
    first.frontmatter.links.push(Link {
        to: Ulid::generate().to_string(),
        rel: Relation::Related,
        reason: Some("Candidate target was removed.".into()),
    });
    write_note(&vault, &first)?;
    write_note(&vault, &second)?;
    index::sync(&vault, false, true)?;
    let report = index::doctor(&vault)?;
    let classes = report
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.class.as_str())
        .collect::<Vec<_>>();
    assert!(classes.contains(&"dependency_cycle"));
    assert!(classes.contains(&"unresolved_dependency"));
    assert!(classes.contains(&"dangling_link"));
    assert!(!report.ok);
    Ok(())
}

#[test]
fn promotion_requires_apply_and_preserves_origin() -> crate::error::Result<()> {
    let (_temporary, _home, vault) = fixture()?;
    let source = note(
        NoteType::Fleeting,
        NoteState::Inbox,
        "Capture boundary",
        "Markdown is the durable capture boundary.",
    );
    let source_id = source.frontmatter.id.clone();
    write_note(&vault, &source)?;
    index::sync(&vault, false, true)?;

    let proposal = mutation::promote(
        &vault,
        std::slice::from_ref(&source_id),
        false,
        "2026-07-29T11:00:00Z",
    )?;
    assert!(!proposal.applied);
    assert!(proposal.created.is_empty());
    assert_eq!(index::all_notes(&vault)?.len(), 1);

    let applied = mutation::promote(
        &vault,
        std::slice::from_ref(&source_id),
        true,
        "2026-07-29T11:00:00Z",
    )?;
    assert!(applied.applied);
    assert_eq!(applied.created.len(), 1);
    assert_eq!(index::all_notes(&vault)?.len(), 2);
    let source_edges = index::edges_from(&vault, &source_id)?;
    assert!(
        source_edges
            .iter()
            .any(|edge| edge.relation == "promoted_to")
    );
    assert!(index::note_path(&vault, &source_id)?.is_file());
    Ok(())
}

#[test]
fn hook_install_and_uninstall_preserve_unrelated_configuration() -> crate::error::Result<()> {
    let temporary = tempfile::tempdir().map_err(|error| crate::MemoryError::io(".", error))?;
    let path = temporary.path().join("codex").join("config.toml");
    fs::create_dir_all(path.parent().unwrap_or_else(|| Path::new(".")))
        .map_err(|error| crate::MemoryError::io(&path, error))?;
    fs::write(
        &path,
        "model = \"gpt-test\"\n[hooks]\nSessionStart = [{ matcher = \"\", hooks = [{ type = \
         \"command\", command = \"echo existing\" }] }]\n",
    )
    .map_err(|error| crate::MemoryError::io(&path, error))?;

    assert!(hook::install_at(path.clone())?.installed);
    assert!(hook::status_at(path.clone())?.installed);
    hook::install_at(path.clone())?;
    let installed =
        fs::read_to_string(&path).map_err(|error| crate::MemoryError::io(&path, error))?;
    assert_eq!(installed.matches("ren-memory-owned:v1").count(), 1);
    assert!(installed.contains("echo existing"));
    assert!(installed.contains("gpt-test"));

    assert!(!hook::uninstall_at(path.clone())?.installed);
    let uninstalled =
        fs::read_to_string(&path).map_err(|error| crate::MemoryError::io(&path, error))?;
    assert!(!uninstalled.contains("ren-memory-owned:v1"));
    assert!(uninstalled.contains("echo existing"));
    assert!(uninstalled.contains("gpt-test"));
    Ok(())
}

#[test]
fn note_serialization_preserves_indentation_and_enforces_size_and_timestamps()
-> crate::error::Result<()> {
    let id = Ulid::generate().to_string();
    let path = Path::new("permanent").join(format!("{id}.md"));
    let input = format!(
        "---\nschema: {SCHEMA}\nid: {id}\ntype: permanent\nstate: accepted\ncreated_at: \
         2026-07-29T10:00:00+09:00\n---\n    indented code\n"
    );
    let parsed = Note::parse(&path, &input)?;
    assert_eq!(parsed.body, "    indented code\n");
    assert_eq!(
        Note::parse(&path, &parsed.to_markdown()?)?.body,
        "    indented code\n"
    );

    let mut oversized = note(NoteType::Permanent, NoteState::Accepted, "Oversized", "");
    oversized.body = "x".repeat(crate::MAX_NOTE_BYTES);
    assert!(matches!(
        oversized.to_markdown(),
        Err(crate::MemoryError::InputTooLarge { .. })
    ));

    let mut invalid_time = note(NoteType::Permanent, NoteState::Accepted, "Bad time", "body");
    invalid_time.frontmatter.created_at = "next Tuesday".into();
    assert!(invalid_time.frontmatter.validate().is_err());
    Ok(())
}

#[test]
fn every_frontmatter_link_requires_a_reason() {
    let mut source = note(NoteType::Permanent, NoteState::Proposed, "Proposed", "body");
    source.frontmatter.links.push(Link {
        to: Ulid::generate().to_string(),
        rel: Relation::Related,
        reason: None,
    });
    assert!(source.frontmatter.validate().is_err());
}

#[test]
fn external_dependencies_are_explicit_and_not_reported_as_dangling() -> crate::error::Result<()> {
    let (_temporary, _home, vault) = fixture()?;
    let mut source = note(
        NoteType::Permanent,
        NoteState::Accepted,
        "External dependency",
        "body",
    );
    source.frontmatter.deps.push(Dependency::External {
        external: "https://example.test/spec".into(),
    });
    write_note(&vault, &source)?;
    index::sync(&vault, false, true)?;
    let report = index::doctor(&vault)?;
    assert!(
        !report
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.class == "unresolved_dependency")
    );
    Ok(())
}

#[test]
fn accepted_mutations_reject_an_invalid_local_dependency_dag() -> crate::error::Result<()> {
    let (_temporary, _home, vault) = fixture()?;
    let mut first = note(NoteType::Permanent, NoteState::Accepted, "First", "body");
    let mut second = note(NoteType::Permanent, NoteState::Accepted, "Second", "body");
    let first_id = first.frontmatter.id.clone();
    first
        .frontmatter
        .deps
        .push(Dependency::Local(second.frontmatter.id.clone()));
    second
        .frontmatter
        .deps
        .push(Dependency::Local(first_id.clone()));
    write_note(&vault, &first)?;
    write_note(&vault, &second)?;
    index::sync(&vault, false, true)?;
    assert!(
        mutation::revise(
            &vault,
            &first_id,
            mutation::TitleChange::Set("Rejected revision".into()),
            None,
            "2026-07-29T12:00:00Z",
        )
        .is_err_and(|error| error.to_string().contains("cycle"))
    );
    Ok(())
}

#[test]
fn index_hash_detects_same_size_same_mtime_replacement() -> crate::error::Result<()> {
    let (_temporary, _home, vault) = fixture()?;
    let source = note(
        NoteType::Permanent,
        NoteState::Accepted,
        "Hash authority",
        "AAAA",
    );
    let path = write_note(&vault, &source)?;
    index::sync(&vault, false, true)?;
    let modified = fs::metadata(&path)
        .map_err(|error| crate::MemoryError::io(&path, error))?
        .modified()
        .map_err(|error| crate::MemoryError::io(&path, error))?;
    let original =
        fs::read_to_string(&path).map_err(|error| crate::MemoryError::io(&path, error))?;
    let replacement = original.replace("AAAA", "BBBB");
    assert_eq!(original.len(), replacement.len());
    let mut file = fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&path)
        .map_err(|error| crate::MemoryError::io(&path, error))?;
    file.write_all(replacement.as_bytes())
        .map_err(|error| crate::MemoryError::io(&path, error))?;
    file.set_times(fs::FileTimes::new().set_modified(modified))
        .map_err(|error| crate::MemoryError::io(&path, error))?;

    let report = index::sync(&vault, false, true)?;
    assert_eq!(report.indexed, 1);
    assert_eq!(
        index::search(&vault, "BBBB", 10)?
            .first()
            .map(|hit| hit.id.as_str()),
        Some(source.frontmatter.id.as_str())
    );
    Ok(())
}

#[test]
fn archive_moves_an_indexed_id_without_rebuild() -> crate::error::Result<()> {
    let (_temporary, _home, vault) = fixture()?;
    let source = note(NoteType::Fleeting, NoteState::Inbox, "Archive me", "body");
    let id = source.frontmatter.id.clone();
    write_note(&vault, &source)?;
    index::sync(&vault, false, true)?;
    let result = mutation::archive(&vault, &id, "2026-07-29T12:00:00Z")?;
    assert_eq!(result.action, "archived");
    assert!(result.path.starts_with(vault.root.join("archived")));
    assert_eq!(index::note_path(&vault, &id)?, result.path);
    Ok(())
}

#[test]
fn invalid_diagnostics_clear_when_fixed_or_deleted() -> crate::error::Result<()> {
    let (_temporary, _home, vault) = fixture()?;
    let valid = note(
        NoteType::Permanent,
        NoteState::Accepted,
        "Eventually valid",
        "body",
    );
    let path = vault.safe_note_path("permanent", &valid.frontmatter.id)?;
    fs::write(&path, "not frontmatter").map_err(|error| crate::MemoryError::io(&path, error))?;
    assert_eq!(index::sync(&vault, false, true)?.invalid.len(), 1);
    fs::write(&path, valid.to_markdown()?).map_err(|error| crate::MemoryError::io(&path, error))?;
    assert!(index::sync(&vault, false, true)?.invalid.is_empty());
    assert!(!index::doctor(&vault)?.diagnostics.iter().any(
        |diagnostic| diagnostic.path.as_deref() == Some(path.as_path())
            && diagnostic.severity == "error"
    ));

    fs::write(&path, "invalid again").map_err(|error| crate::MemoryError::io(&path, error))?;
    index::sync(&vault, false, true)?;
    fs::remove_file(&path).map_err(|error| crate::MemoryError::io(&path, error))?;
    index::sync(&vault, false, true)?;
    assert!(
        !index::doctor(&vault)?
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.path.as_deref() == Some(path.as_path()))
    );
    Ok(())
}

#[test]
fn traversal_ignores_dangling_phantom_nodes() -> crate::error::Result<()> {
    let (_temporary, _home, vault) = fixture()?;
    let target = note(NoteType::Permanent, NoteState::Accepted, "Target", "body");
    let target_id = target.frontmatter.id.clone();
    write_note(&vault, &target)?;
    let dangling = Ulid::generate().to_string();
    let mut source = note(NoteType::Permanent, NoteState::Accepted, "Source", "body");
    let source_id = source.frontmatter.id.clone();
    source.frontmatter.links.extend([
        Link {
            to: target_id,
            rel: Relation::Related,
            reason: Some("Resolved neighbor.".into()),
        },
        Link {
            to: dangling.clone(),
            rel: Relation::Related,
            reason: Some("Missing neighbor.".into()),
        },
    ]);
    write_note(&vault, &source)?;
    index::sync(&vault, false, true)?;
    assert!(
        index::related(&vault, &source_id, 2)?
            .iter()
            .all(|edge| edge.to != dangling && edge.from != dangling)
    );
    Ok(())
}

#[test]
fn hook_uninstall_preserves_other_handlers_in_the_same_group() -> crate::error::Result<()> {
    let temporary = tempfile::tempdir().map_err(|error| crate::MemoryError::io(".", error))?;
    let path = temporary.path().join("config.toml");
    fs::write(
        &path,
        "[hooks]\nStop = [{ matcher = \"\", hooks = [{ type = \"command\", command = \"echo \
         keep\" }, { type = \"command\", command = \"ren memory ingest-hook --agent codex --event \
         stop --quiet\", statusMessage = \"ren-memory-owned:v1\" }] }]\n",
    )
    .map_err(|error| crate::MemoryError::io(&path, error))?;
    hook::uninstall_at(path.clone())?;
    let output = fs::read_to_string(&path).map_err(|error| crate::MemoryError::io(&path, error))?;
    assert!(output.contains("echo keep"));
    assert!(!output.contains("ren-memory-owned:v1"));
    Ok(())
}

#[test]
fn hook_hint_does_not_fall_back_to_an_unrelated_single_vault() -> crate::error::Result<()> {
    let (temporary, home, vault) = fixture()?;
    let unrelated = temporary.path().join("unrelated");
    fs::create_dir(&unrelated).map_err(|error| crate::MemoryError::io(&unrelated, error))?;
    assert!(matches!(
        home.resolve_or_register_hint(&unrelated),
        Err(crate::MemoryError::VaultNotFound)
    ));
    assert_eq!(home.load_registry()?.vaults.len(), 1);
    assert_eq!(home.resolve(None, &unrelated)?.id, vault.id);
    Ok(())
}

#[test]
fn concurrent_registry_updates_are_not_lost() -> crate::error::Result<()> {
    let temporary = tempfile::tempdir().map_err(|error| crate::MemoryError::io(".", error))?;
    let home = Arc::new(MemoryHome {
        root: temporary.path().join("memory"),
    });
    let barrier = Arc::new(Barrier::new(6));
    let mut threads = Vec::new();
    for index in 0..6 {
        let project = temporary.path().join(format!("project-{index}"));
        fs::create_dir(&project).map_err(|error| crate::MemoryError::io(&project, error))?;
        let home = Arc::clone(&home);
        let barrier = Arc::clone(&barrier);
        threads.push(std::thread::spawn(move || {
            barrier.wait();
            home.register(Some(&format!("vault-{index}")), None, &project)
        }));
    }
    for thread in threads {
        thread
            .join()
            .map_err(|_| crate::MemoryError::Validation("registration thread panicked".into()))??;
    }
    assert_eq!(home.load_registry()?.vaults.len(), 6);
    Ok(())
}

#[test]
fn manual_title_is_metadata_and_common_credential_forms_are_redacted() -> crate::error::Result<()> {
    let (_temporary, _home, vault) = fixture()?;
    let event = crate::capture::manual_event(
        &vault,
        "Body starts here.\n\"api_key\": \"json-secret\"\ntoken = plain-secret\nAuthorization: \
         Bearer bearer-secret\nghp_abcdefghijklmnop",
        Some("Manual title"),
        "2026-07-29T10:00:00Z".into(),
    );
    let result = capture_event(&vault, &event)?;
    let markdown = fs::read_to_string(&result.path)
        .map_err(|error| crate::MemoryError::io(&result.path, error))?;
    let captured = Note::parse(&result.path, &markdown)?;
    assert_eq!(captured.frontmatter.title.as_deref(), Some("Manual title"));
    assert!(captured.body.starts_with("Body starts here."));
    assert!(!captured.body.starts_with("Manual title"));
    for secret in [
        "json-secret",
        "plain-secret",
        "bearer-secret",
        "ghp_abcdefghijklmnop",
    ] {
        assert!(!markdown.contains(secret));
        assert!(index::search(&vault, secret, 10)?.is_empty());
    }
    Ok(())
}

#[test]
fn promotion_is_persisted_stale_safe_idempotent_and_rebuildable() -> crate::error::Result<()> {
    let (_temporary, _home, vault) = fixture()?;
    let stale_source = note(
        NoteType::Fleeting,
        NoteState::Inbox,
        "Stale source",
        "first revision",
    );
    let stale_id = stale_source.frontmatter.id.clone();
    write_note(&vault, &stale_source)?;
    index::sync(&vault, false, true)?;
    let stale_proposal = mutation::promote(
        &vault,
        std::slice::from_ref(&stale_id),
        false,
        "2026-07-29T11:00:00Z",
    )?;
    mutation::revise(
        &vault,
        &stale_id,
        mutation::TitleChange::Unchanged,
        Some("second revision".into()),
        "2026-07-29T11:30:00Z",
    )?;
    assert!(
        mutation::promote_operation(
            &vault,
            std::slice::from_ref(&stale_id),
            Some(&stale_proposal.operation_key),
            true,
            "2026-07-29T12:00:00Z",
        )
        .is_err_and(|error| error.to_string().contains("stale"))
    );

    let source = note(
        NoteType::Fleeting,
        NoteState::Inbox,
        "Fresh source",
        "durable observation",
    );
    let source_id = source.frontmatter.id.clone();
    write_note(&vault, &source)?;
    index::sync(&vault, false, true)?;
    let proposal = mutation::promote(
        &vault,
        std::slice::from_ref(&source_id),
        false,
        "2026-07-29T12:00:00Z",
    )?;
    let first = mutation::promote_operation(
        &vault,
        std::slice::from_ref(&source_id),
        Some(&proposal.operation_key),
        true,
        "2026-07-29T12:05:00Z",
    )?;
    let retry = mutation::promote_operation(
        &vault,
        std::slice::from_ref(&source_id),
        Some(&proposal.operation_key),
        true,
        "2026-07-29T12:10:00Z",
    )?;
    assert_eq!(first.operation_key, retry.operation_key);
    assert_eq!(first.created[0].note_id, retry.created[0].note_id);
    assert_eq!(index::all_notes(&vault)?.len(), 3);
    assert!(
        fs::read_dir(vault.index_dir().join("transactions"))
            .map_err(|error| {
                crate::MemoryError::io(vault.index_dir().join("transactions"), error)
            })?
            .next()
            .is_none()
    );

    index::sync(&vault, true, true)?;
    let connection = index::open_writer(&vault)?;
    let revisions: i64 =
        connection.query_row("SELECT COUNT(*) FROM revisions", [], |row| row.get(0))?;
    assert!(revisions >= 2);
    Ok(())
}

#[test]
fn project_promotion_workflow_is_invoked_and_validated() -> crate::error::Result<()> {
    let (_temporary, _home, vault) = fixture()?;
    let workflow_dir = vault.project_path.join(".ren").join("workflows");
    fs::create_dir_all(&workflow_dir)
        .map_err(|error| crate::MemoryError::io(&workflow_dir, error))?;
    let workflow = workflow_dir.join("zettelkasten-promote.rhai");
    fs::write(
        &workflow,
        r#"let meta = #{
    name: "zettelkasten-promote",
    description: "test override",
    when_to_use: "test",
    args_schema: #{ type: "object", additionalProperties: true }
};
let input = args.inputs[0];
complete(#{
    schema: "ren-memory-promotion-proposal/v1",
    proposals: [#{
        source_id: input.source_id,
        source_revision: input.source_revision,
        target_id: input.target_id,
        target_type: "literature",
        title: "Workflow-generated title",
        body: input.body,
        rationale: "Project workflow override.",
        sources: input.sources,
        links: []
    }]
});"#,
    )
    .map_err(|error| crate::MemoryError::io(&workflow, error))?;
    let source = note(NoteType::Fleeting, NoteState::Inbox, "Original", "body");
    let id = source.frontmatter.id.clone();
    write_note(&vault, &source)?;
    index::sync(&vault, false, true)?;
    let proposal = mutation::promote(&vault, &[id], false, "2026-07-29T12:00:00Z")?;
    assert_eq!(proposal.proposals[0].target_type, "literature");
    assert_eq!(
        proposal.proposals[0].title.as_deref(),
        Some("Workflow-generated title")
    );
    Ok(())
}

#[test]
fn doctor_reports_a_missing_disposable_index() -> crate::error::Result<()> {
    let (_temporary, _home, vault) = fixture()?;
    let report = index::doctor(&vault)?;
    assert!(!report.ok);
    assert_eq!(report.indexed_notes, 0);
    assert!(
        report
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.class == "missing_index")
    );
    Ok(())
}

#[test]
fn search_ranking_rewards_exact_tags_and_source_quality() -> crate::error::Result<()> {
    let (_temporary, _home, vault) = fixture()?;
    let plain = note(
        NoteType::Permanent,
        NoteState::Accepted,
        "Plain result",
        "catalog ranking signal",
    );
    write_note(&vault, &plain)?;
    let mut boosted = note(
        NoteType::Permanent,
        NoteState::Accepted,
        "Boosted result",
        "catalog ranking signal",
    );
    boosted.frontmatter.tags.push("catalog".into());
    boosted.frontmatter.sources.push(Source {
        kind: "literature".into(),
        fields: std::collections::BTreeMap::default(),
    });
    let boosted_id = boosted.frontmatter.id.clone();
    write_note(&vault, &boosted)?;
    index::sync(&vault, false, true)?;
    let hits = index::search(&vault, "catalog", 10)?;
    assert_eq!(
        hits.first().map(|hit| hit.id.as_str()),
        Some(boosted_id.as_str())
    );
    Ok(())
}

#[test]
fn hook_policy_denies_registered_paths_and_allows_explicit_auto_registration()
-> crate::error::Result<()> {
    let (temporary, home, vault) = fixture()?;
    let config_path = home.root.join("config.toml");
    fs::write(
        &config_path,
        format!(
            "schema = \"ren-memory-config/v1\"\nredact_secrets = true\n\n[hooks]\n\
             auto_register_unmatched = false\nallow_paths = []\ndeny_paths = [{:?}]\n",
            vault.project_path.to_string_lossy()
        ),
    )
    .map_err(|error| crate::MemoryError::io(&config_path, error))?;
    assert!(matches!(
        home.resolve_or_register_hint(&vault.project_path),
        Err(crate::MemoryError::UnsafeInput(_))
    ));

    let allowed = temporary.path().join("allowed");
    fs::create_dir(&allowed).map_err(|error| crate::MemoryError::io(&allowed, error))?;
    fs::write(
        &config_path,
        format!(
            "schema = \"ren-memory-config/v1\"\nredact_secrets = true\n\n[hooks]\n\
             auto_register_unmatched = true\nallow_paths = [{:?}]\ndeny_paths = []\n",
            allowed.to_string_lossy()
        ),
    )
    .map_err(|error| crate::MemoryError::io(&config_path, error))?;
    assert_eq!(
        home.resolve_or_register_hint(&allowed)?.project_path,
        fs::canonicalize(&allowed).map_err(|error| crate::MemoryError::io(&allowed, error))?
    );
    Ok(())
}

#[test]
fn multi_note_promotion_applies_as_one_recoverable_operation() -> crate::error::Result<()> {
    let (_temporary, _home, vault) = fixture()?;
    let first = note(
        NoteType::Fleeting,
        NoteState::Inbox,
        "First source",
        "first durable idea",
    );
    let first_id = first.frontmatter.id.clone();
    write_note(&vault, &first)?;
    let second = note(
        NoteType::Fleeting,
        NoteState::Inbox,
        "Second source",
        "second durable idea",
    );
    let second_id = second.frontmatter.id.clone();
    write_note(&vault, &second)?;
    index::sync(&vault, false, true)?;
    let ids = vec![first_id.clone(), second_id.clone()];
    let proposal = mutation::promote(&vault, &ids, false, "2026-07-29T12:00:00Z")?;
    let applied = mutation::promote_operation(
        &vault,
        &ids,
        Some(&proposal.operation_key),
        true,
        "2026-07-29T12:05:00Z",
    )?;
    assert_eq!(applied.created.len(), 2);
    assert_eq!(index::all_notes(&vault)?.len(), 4);
    for id in [first_id, second_id] {
        assert!(
            index::edges_from(&vault, &id)?
                .iter()
                .any(|edge| edge.relation == "promoted_to")
        );
    }
    assert!(
        fs::read_dir(vault.index_dir().join("transactions"))
            .map_err(|error| {
                crate::MemoryError::io(vault.index_dir().join("transactions"), error)
            })?
            .next()
            .is_none()
    );
    Ok(())
}

#[test]
fn promotion_uses_raw_revisions_and_deduplicates_selected_ids() -> crate::error::Result<()> {
    let (_temporary, _home, vault) = fixture()?;
    let source = note(
        NoteType::Fleeting,
        NoteState::Inbox,
        "Non-canonical source",
        "Raw bytes are the revision boundary.\n",
    );
    let id = source.frontmatter.id.clone();
    let path = vault.safe_note_path("fleeting", &id)?;
    let noncanonical = source.to_markdown()?.replace('\n', "\r\n");
    publish_new(&path, noncanonical.as_bytes())?;
    index::sync(&vault, false, true)?;

    let duplicate_ids = vec![id.clone(), id];
    let proposal = mutation::promote(&vault, &duplicate_ids, false, "2026-07-29T12:00:00Z")?;
    assert_eq!(proposal.actions.len(), 1);
    let applied = mutation::promote_operation(
        &vault,
        &[],
        Some(&proposal.operation_key),
        true,
        "2026-07-29T12:05:00Z",
    )?;
    assert_eq!(applied.created.len(), 1);
    Ok(())
}

#[test]
fn promote_apply_operation_parses_without_positional_ids() {
    use std::ffi::OsStr;
    use usage::{Cli, Subcommands};

    #[derive(Debug, Cli)]
    struct TestCli {
        #[usage(subcommand)]
        command: TestCommand,
    }

    #[derive(Debug, Subcommands)]
    enum TestCommand {
        Memory(crate::Config),
    }

    let parsed = TestCli::try_parse_from(&[
        OsStr::new("ren"),
        OsStr::new("memory"),
        OsStr::new("promote"),
        OsStr::new("--apply"),
        OsStr::new("--operation"),
        OsStr::new("abc"),
    ]);
    assert!(parsed.is_ok());
}

#[test]
fn capture_projection_failure_keeps_durable_note_and_receipt() -> crate::error::Result<()> {
    let (_temporary, _home, vault) = fixture()?;
    let duplicate = note(
        NoteType::Permanent,
        NoteState::Accepted,
        "Duplicate",
        "duplicate",
    );
    let duplicate_id = duplicate.frontmatter.id.clone();
    let permanent = vault.safe_note_path("permanent", &duplicate_id)?;
    let archived = vault.safe_note_path("archived", &duplicate_id)?;
    publish_new(&permanent, duplicate.to_markdown()?.as_bytes())?;
    publish_new(&archived, duplicate.to_markdown()?.as_bytes())?;

    let event = crate::capture::manual_event(
        &vault,
        "This capture remains durable.",
        None,
        "2026-07-29T10:00:00Z".into(),
    );
    let result = capture_event(&vault, &event)?;
    assert!(result.captured);
    assert!(!result.indexed);
    assert!(result.path.is_file());
    assert!(
        vault
            .index_dir()
            .join("capture-spool")
            .join(format!("{}.json", result.event_key))
            .is_file()
    );
    assert!(
        fs::read_dir(vault.index_dir().join("capture-spool"))
            .map_err(|error| crate::MemoryError::io(vault.index_dir(), error))?
            .all(|entry| entry.is_ok_and(|entry| {
                entry.path().extension().and_then(|value| value.to_str()) == Some("json")
            }))
    );
    Ok(())
}

#[test]
fn search_falls_back_for_unmatched_fts_quotes() -> crate::error::Result<()> {
    let (_temporary, _home, vault) = fixture()?;
    let source = note(
        NoteType::Permanent,
        NoteState::Accepted,
        "Quoted",
        "A literal \"quoted fragment is searchable.",
    );
    let id = source.frontmatter.id.clone();
    write_note(&vault, &source)?;
    index::sync(&vault, false, true)?;
    let hits = index::search(&vault, "\"quoted", 10)?;
    assert!(hits.iter().any(|hit| hit.id == id));
    Ok(())
}

#[test]
fn doctor_structures_partial_and_future_schema_failures() -> crate::error::Result<()> {
    let (_temporary, _home, vault) = fixture()?;
    {
        let connection = rusqlite::Connection::open(vault.database_path())?;
        connection.execute_batch("CREATE TABLE unrelated(value TEXT);")?;
    }
    let partial = index::doctor(&vault)?;
    assert!(!partial.ok);
    assert!(
        partial
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.class == "schema_inspection")
    );

    fs::remove_file(vault.database_path())
        .map_err(|error| crate::MemoryError::io(vault.database_path(), error))?;
    let connection = index::open_writer(&vault)?;
    connection.execute(
        "UPDATE memory_meta SET value = '999' WHERE key = 'schema_version'",
        [],
    )?;
    drop(connection);
    let future = index::doctor(&vault)?;
    assert!(!future.ok);
    assert_eq!(future.schema_version, 999);
    assert!(
        future
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.class == "schema_version")
    );
    assert!(index::open_writer(&vault).is_err());
    Ok(())
}

#[test]
fn promotion_workflows_with_agent_calls_are_rejected() -> crate::error::Result<()> {
    let (_temporary, _home, vault) = fixture()?;
    let workflow_dir = vault.project_path.join(".ren").join("workflows");
    fs::create_dir_all(&workflow_dir)
        .map_err(|error| crate::MemoryError::io(&workflow_dir, error))?;
    let workflow = workflow_dir.join("zettelkasten-promote.rhai");
    fs::write(
        &workflow,
        r#"let meta = #{
    name: "zettelkasten-promote",
    description: "must be rejected",
    when_to_use: "test",
    args_schema: #{ type: "object", additionalProperties: true }
};
let ignored = agent("synthetic proposal");
let input = args.inputs[0];
complete(#{
    schema: "ren-memory-promotion-proposal/v1",
    proposals: [#{
        source_id: input.source_id,
        source_revision: input.source_revision,
        target_id: input.target_id,
        target_type: "permanent",
        title: input.title,
        body: input.body,
        rationale: "invalid synthetic result",
        sources: input.sources,
        links: []
    }]
});"#,
    )
    .map_err(|error| crate::MemoryError::io(&workflow, error))?;
    let source = note(NoteType::Fleeting, NoteState::Inbox, "Source", "body");
    let id = source.frontmatter.id.clone();
    write_note(&vault, &source)?;
    index::sync(&vault, false, true)?;
    assert!(
        mutation::promote(&vault, &[id], false, "2026-07-29T12:00:00Z")
            .is_err_and(|error| error.to_string().contains("agent()"))
    );
    Ok(())
}

#[test]
fn tagged_actions_support_multi_source_creation_and_archive() -> crate::error::Result<()> {
    let (_temporary, _home, vault) = fixture()?;
    let workflow_dir = vault.project_path.join(".ren").join("workflows");
    fs::create_dir_all(&workflow_dir)
        .map_err(|error| crate::MemoryError::io(&workflow_dir, error))?;
    let workflow = workflow_dir.join("zettelkasten-promote.rhai");
    fs::write(
        &workflow,
        r#"let meta = #{
    name: "zettelkasten-promote",
    description: "v2 actions",
    when_to_use: "test",
    args_schema: #{ type: "object", additionalProperties: true }
};
let first = args.inputs[0];
let second = args.inputs[1];
complete(#{
    schema: "ren-memory-promotion-proposal/v2",
    actions: [
        #{
            action: "create_note",
            sources: [
                #{ source_id: first.source_id, source_revision: first.source_revision },
                #{ source_id: second.source_id, source_revision: second.source_revision }
            ],
            target_id: first.target_id,
            target_type: "permanent",
            title: "Combined",
            body: "A reviewed multi-source note.",
            rationale: "The sources support one reusable claim.",
            source_locators: [],
            links: []
        },
        #{
            action: "archive",
            sources: [
                #{ source_id: second.source_id, source_revision: second.source_revision }
            ],
            rationale: "The source has been preserved by the durable note."
        }
    ]
});"#,
    )
    .map_err(|error| crate::MemoryError::io(&workflow, error))?;
    let first = note(NoteType::Fleeting, NoteState::Inbox, "First", "first");
    let second = note(NoteType::Fleeting, NoteState::Inbox, "Second", "second");
    let ids = vec![first.frontmatter.id.clone(), second.frontmatter.id.clone()];
    write_note(&vault, &first)?;
    write_note(&vault, &second)?;
    index::sync(&vault, false, true)?;
    let proposal = mutation::promote(&vault, &ids, false, "2026-07-29T12:00:00Z")?;
    let applied = mutation::promote_operation(
        &vault,
        &[],
        Some(&proposal.operation_key),
        true,
        "2026-07-29T12:05:00Z",
    )?;
    assert_eq!(applied.created.len(), 1);
    let promoted = crate::model::read_note(&applied.created[0].path)?;
    assert_eq!(promoted.frontmatter.promoted_from.len(), 2);
    assert_eq!(
        ids.iter()
            .filter_map(|id| vault.safe_note_path("archived", id).ok())
            .filter(|path| path.is_file())
            .count(),
        1
    );
    Ok(())
}

#[test]
fn frontmatter_provenance_ids_are_validated_deduplicated_and_projected() -> crate::error::Result<()>
{
    let (_temporary, _home, vault) = fixture()?;
    let source = note(NoteType::Permanent, NoteState::Accepted, "Source", "source");
    let source_id = source.frontmatter.id.clone();
    write_note(&vault, &source)?;
    let mut derived = note(
        NoteType::Permanent,
        NoteState::Accepted,
        "Derived",
        "derived",
    );
    derived.frontmatter.promoted_from = vec![source_id.clone(), source_id.clone()];
    derived.frontmatter.supersedes = vec![source_id; 2];
    derived.frontmatter.validate()?;
    assert_eq!(derived.frontmatter.promoted_from.len(), 1);
    assert_eq!(derived.frontmatter.supersedes.len(), 1);
    let derived_id = derived.frontmatter.id.clone();
    write_note(&vault, &derived)?;
    index::sync(&vault, false, true)?;
    let edges = index::edges_from(&vault, &derived_id)?;
    assert!(edges.iter().any(|edge| edge.relation == "source_of"));
    assert!(edges.iter().any(|edge| edge.relation == "supersedes"));

    derived.frontmatter.promoted_from = vec!["../escape".into()];
    assert!(derived.frontmatter.validate().is_err());
    Ok(())
}

#[test]
fn registration_checks_conflicts_before_creating_layout() -> crate::error::Result<()> {
    let (temporary, home, vault) = fixture()?;
    let unused = temporary.path().join("must-not-be-created");
    assert!(
        home.register(Some(&vault.id), Some(&unused), &vault.project_path)
            .is_err()
    );
    assert!(!unused.exists());
    let indexes = fs::canonicalize(home.root.join("indexes"))
        .map_err(|error| crate::MemoryError::io(&home.root, error))?;
    assert!(vault.index_dir().starts_with(indexes));
    assert!(!vault.index_dir().starts_with(&vault.root));
    Ok(())
}

#[test]
fn embedded_memory_skill_has_matching_content_and_installs_all_files()
-> Result<(), ren_workflow::WorkflowError> {
    let frontmatter = crate::MEMORY_SKILL_MD
        .strip_prefix("---\n")
        .and_then(|skill| skill.split_once("\n---\n"))
        .map(|(frontmatter, _)| frontmatter)
        .ok_or_else(|| {
            ren_workflow::WorkflowError::InvalidConfig(
                "memory skill frontmatter is not closed".into(),
            )
        })?;
    let keys = frontmatter
        .lines()
        .filter(|line| !line.chars().next().is_some_and(char::is_whitespace))
        .filter_map(|line| line.split_once(':').map(|(key, _)| key))
        .collect::<Vec<_>>();
    assert_eq!(keys, ["name", "description"]);
    assert!(frontmatter.contains("name: ren-memory"));
    assert!(frontmatter.contains("remember this"));
    assert!(crate::MEMORY_SKILL_MD.contains("ren memory --help"));
    assert!(crate::MEMORY_SKILL_MD.contains("ren memory <subcommand> --help"));
    assert!(
        crate::MEMORY_SKILL_MD.contains(
            "ren memory init --user\nren memory index --rebuild\nren memory hook install --agent \
             codex --user"
        ),
        "Codex hook setup commands must be documented in execution order"
    );
    assert!(crate::MEMORY_SKILL_MD.contains("$CODEX_HOME/config.toml"));
    assert!(crate::MEMORY_SKILL_MD.contains("$HOME/.codex/config.toml"));
    assert!(crate::MEMORY_SKILL_MD.contains("ren init"));
    assert!(crate::MEMORY_SKILL_MD.contains("# ren-memory"));
    assert!(
        crate::MEMORY_SKILL_MD.contains("local ren-memory vault"),
        "component prose must use the hyphenated skill name"
    );
    assert_eq!(
        yaml_serde::from_str::<OpenAiMetadata>(crate::MEMORY_OPENAI_YAML)
            .map_err(|error| { ren_workflow::WorkflowError::InvalidConfig(error.to_string()) })?,
        OpenAiMetadata {
            interface: OpenAiInterface {
                display_name: "ren-memory".into(),
                short_description: "Capture, search, and curate local agent memory".into(),
                default_prompt: "Use $ren-memory to capture and retrieve project knowledge.".into(),
            },
        }
    );
    let user_facing_assets = [
        ("README.md", include_str!("../../README.md")),
        ("ren-memory/SKILL.md", crate::MEMORY_SKILL_MD),
        ("ren-memory/agents/openai.yaml", crate::MEMORY_OPENAI_YAML),
    ];
    for legacy in [
        "ren Memory",
        "ren Workflow",
        "# ren memory",
        "# ren workflow",
    ] {
        for (name, contents) in user_facing_assets {
            assert!(
                !contents.contains(legacy),
                "legacy component display form `{legacy}` remains in {name}"
            );
        }
    }
    assert!(include_str!("../../README.md").contains("## ren-memory"));
    assert!(include_str!("../../README.md").contains("`ren memory`"));

    #[cfg(unix)]
    {
        let base = tempfile::tempdir()?;
        for agent in ren_workflow::supported_agents() {
            let definition = ren_workflow::skill_definition_for(
                base.path(),
                ren_workflow::InitScope::User,
                agent,
                crate::MEMORY_SKILL,
            );
            assert!(definition.dir.ends_with("skills/ren-memory"));
            assert_eq!(definition.files.len(), crate::MEMORY_SKILL_FILES.len());
            ren_workflow::install_skill(&definition, false)?;
            assert_eq!(
                fs::read_to_string(definition.dir.join("SKILL.md"))?,
                crate::MEMORY_SKILL_MD
            );
            assert_eq!(
                fs::read_to_string(definition.dir.join("agents/openai.yaml"))?,
                crate::MEMORY_OPENAI_YAML
            );
        }
    }
    Ok(())
}

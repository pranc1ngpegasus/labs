use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    fs,
    path::{Component, Path, PathBuf},
};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use crate::{
    error::{MemoryError, Result},
    fsutil::{create_private_dir, publish_new, write_atomic_replace},
    index,
    model::{Link, Note, NoteState, NoteType, Relation, read_note},
    vault::Vault,
};

const PROMOTION_SCHEMA: &str = "ren-memory-promotion/v1";
const PROPOSAL_SCHEMA: &str = "ren-memory-promotion-proposal/v1";
const ACTION_PROPOSAL_SCHEMA: &str = "ren-memory-promotion-proposal/v2";
const PROMOTION_WORKFLOW: &str = include_str!("../bundled/zettelkasten-promote.rhai");

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct MutationResult {
    pub action: String,
    pub note_id: String,
    pub path: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PromotionProposal {
    pub source_id: String,
    pub source_revision: String,
    pub target_id: String,
    pub target_type: String,
    pub title: Option<String>,
    pub body: String,
    pub rationale: String,
    #[serde(default)]
    pub sources: Vec<crate::model::Source>,
    #[serde(default)]
    pub links: Vec<Link>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PromotionSourceRevision {
    pub source_id: String,
    pub source_revision: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum PromotionAction {
    KeepFleeting {
        sources: Vec<PromotionSourceRevision>,
        rationale: String,
    },
    NeedsContext {
        sources: Vec<PromotionSourceRevision>,
        rationale: String,
    },
    Archive {
        sources: Vec<PromotionSourceRevision>,
        rationale: String,
    },
    CreateNote {
        sources: Vec<PromotionSourceRevision>,
        target_id: String,
        target_type: String,
        title: Option<String>,
        body: String,
        rationale: String,
        #[serde(default)]
        source_locators: Vec<crate::model::Source>,
        #[serde(default)]
        links: Vec<Link>,
    },
    UpsertCollection {
        sources: Vec<PromotionSourceRevision>,
        target_id: String,
        target_revision: Option<String>,
        target_type: String,
        title: Option<String>,
        body: String,
        rationale: String,
        #[serde(default)]
        source_locators: Vec<crate::model::Source>,
        #[serde(default)]
        links: Vec<Link>,
    },
    SuggestLink {
        sources: Vec<PromotionSourceRevision>,
        from_id: String,
        to_id: String,
        relation: Relation,
        reason: String,
    },
    Duplicate {
        sources: Vec<PromotionSourceRevision>,
        candidate_id: String,
        exact: bool,
        rationale: String,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PromotionResult {
    pub operation_key: String,
    pub applied: bool,
    #[serde(default)]
    pub actions: Vec<PromotionAction>,
    /// Legacy create-note view retained for pipe compatibility with v1.
    #[serde(default)]
    pub proposals: Vec<PromotionProposal>,
    pub created: Vec<MutationResult>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct WorkflowProposal {
    schema: String,
    actions: Vec<PromotionAction>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct PromotionInput {
    source_id: String,
    source_revision: String,
    target_id: String,
    title: Option<String>,
    body: String,
    sources: Vec<crate::model::Source>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct WorkflowArgs {
    schema: String,
    inputs: Vec<PromotionInput>,
    nearby_notes: Vec<NearbyNote>,
    exact_duplicates: Vec<DuplicateCandidate>,
    similarity_candidates: Vec<SimilarityCandidate>,
    graph_neighborhood: Vec<WorkflowEdge>,
    policy: serde_json::Value,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct NearbyNote {
    id: String,
    note_type: String,
    title: Option<String>,
    body: String,
    body_hash: String,
    revision: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct DuplicateCandidate {
    source_id: String,
    candidate_id: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct SimilarityCandidate {
    source_id: String,
    candidate_id: String,
    shared_terms: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct WorkflowEdge {
    from: String,
    to: String,
    relation: String,
    reason: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct StoredPromotionInput {
    selected_ids: Vec<String>,
    source_revisions: BTreeMap<String, String>,
    workflow_fingerprint: String,
    workflow_args: WorkflowArgs,
}

#[derive(Clone, Debug)]
struct StoredRun {
    operation_key: String,
    input: StoredPromotionInput,
    proposal: WorkflowProposal,
    result: Option<PromotionResult>,
    state: String,
}

pub enum TitleChange {
    Unchanged,
    Set(String),
    Clear,
}

pub fn link(
    vault: &Vault,
    from: &str,
    relation: Relation,
    to: &str,
    reason: &str,
    now: &str,
) -> Result<MutationResult> {
    if reason.trim().is_empty() {
        return Err(MemoryError::Validation(
            "accepted links require a non-empty reason".into(),
        ));
    }
    index::sync(vault, false, true)?;
    index::validate_dependency_graph(vault)?;
    let from_path = index::note_path(vault, from)?;
    let _ = index::note_path(vault, to)?;
    let writer_lock = vault.lock_writer(true)?;
    let original =
        fs::read_to_string(&from_path).map_err(|error| MemoryError::io(&from_path, error))?;
    let mut note = read_note(&from_path)?;
    if note
        .frontmatter
        .links
        .iter()
        .any(|link| link.to == to && link.rel == relation)
    {
        return Ok(MutationResult {
            action: "unchanged".into(),
            note_id: from.into(),
            path: from_path,
        });
    }
    note.frontmatter.links.push(Link {
        to: to.into(),
        rel: relation,
        reason: Some(reason.into()),
    });
    note.frontmatter.updated_at = Some(now.into());
    note.frontmatter.validate()?;
    let replacement = note.to_markdown()?;
    apply_note_replacements(
        vault,
        &sha256_hex(format!("link:{from}:{to}:{relation}").as_bytes()),
        vec![replacement_change(
            vault,
            &from_path,
            &original,
            replacement,
        )?],
    )?;
    drop(writer_lock);
    index::sync(vault, false, true)?;
    Ok(MutationResult {
        action: "linked".into(),
        note_id: from.into(),
        path: from_path,
    })
}

pub fn revise(
    vault: &Vault,
    id: &str,
    title: TitleChange,
    body: Option<String>,
    now: &str,
) -> Result<MutationResult> {
    if matches!(title, TitleChange::Unchanged) && body.is_none() {
        return Err(MemoryError::Validation(
            "revise requires --title, --clear-title, or --body".into(),
        ));
    }
    index::sync(vault, false, true)?;
    index::validate_dependency_graph(vault)?;
    let path = index::note_path(vault, id)?;
    let writer_lock = vault.lock_writer(true)?;
    let original = fs::read_to_string(&path).map_err(|error| MemoryError::io(&path, error))?;
    let mut note = read_note(&path)?;
    match title {
        TitleChange::Unchanged => {},
        TitleChange::Set(title) => note.frontmatter.title = Some(title),
        TitleChange::Clear => note.frontmatter.title = None,
    }
    if let Some(body) = body {
        if body.trim().is_empty() {
            return Err(MemoryError::Validation(
                "note body must not be empty".into(),
            ));
        }
        note.body = body;
    }
    note.frontmatter.updated_at = Some(now.into());
    note.frontmatter.validate()?;
    let replacement = note.to_markdown()?;
    apply_note_replacements(
        vault,
        &sha256_hex(format!("revise:{id}:{now}").as_bytes()),
        vec![replacement_change(vault, &path, &original, replacement)?],
    )?;
    drop(writer_lock);
    index::sync(vault, false, true)?;
    Ok(MutationResult {
        action: "revised".into(),
        note_id: id.into(),
        path,
    })
}

pub fn archive(
    vault: &Vault,
    id: &str,
    now: &str,
) -> Result<MutationResult> {
    index::sync(vault, false, true)?;
    index::validate_dependency_graph(vault)?;
    let source = index::note_path(vault, id)?;
    let writer_lock = vault.lock_writer(true)?;
    let original = fs::read_to_string(&source).map_err(|error| MemoryError::io(&source, error))?;
    let mut note = read_note(&source)?;
    if note.frontmatter.state == NoteState::Archived {
        return Ok(MutationResult {
            action: "unchanged".into(),
            note_id: id.into(),
            path: source,
        });
    }
    note.frontmatter.state = NoteState::Archived;
    note.frontmatter.updated_at = Some(now.into());
    note.frontmatter.validate()?;
    let destination = vault.safe_note_path("archived", id)?;
    if destination.exists() {
        return Err(MemoryError::Validation(format!(
            "archive destination already exists: {}",
            destination.display()
        )));
    }
    let replacement = note.to_markdown()?;
    let mut changes = vec![
        FileChange {
            target: destination.clone(),
            original: None,
            replacement: Some(replacement.into_bytes()),
        },
        FileChange {
            target: source,
            original: Some(original.as_bytes().to_vec()),
            replacement: None,
        },
    ];
    changes.push(revision_change(vault, id, &original)?);
    apply_file_transaction(
        vault,
        &sha256_hex(format!("archive:{id}:{now}").as_bytes()),
        changes,
    )?;
    drop(writer_lock);
    index::sync(vault, false, true)?;
    Ok(MutationResult {
        action: "archived".into(),
        note_id: id.into(),
        path: destination,
    })
}

#[cfg(test)]
pub fn promote(
    vault: &Vault,
    ids: &[String],
    apply: bool,
    now: &str,
) -> Result<PromotionResult> {
    promote_operation(vault, ids, None, apply, now)
}

pub fn promote_operation(
    vault: &Vault,
    ids: &[String],
    requested_operation: Option<&str>,
    apply: bool,
    now: &str,
) -> Result<PromotionResult> {
    if ids.is_empty() && !(apply && requested_operation.is_some()) {
        return Err(MemoryError::Validation(
            "promote requires note ids or --apply with --operation".into(),
        ));
    }
    index::sync(vault, false, true)?;
    if apply {
        index::validate_dependency_graph(vault)?;
        return apply_promotion(vault, ids, requested_operation);
    }
    if requested_operation.is_some() {
        return Err(MemoryError::Validation(
            "--operation is only valid together with --apply".into(),
        ));
    }

    let (workflow_source, workflow_fingerprint) = promotion_workflow(vault)?;
    let mut selected_ids = ids.to_vec();
    selected_ids.sort();
    selected_ids.dedup();
    let mut workflow_inputs = Vec::new();
    let mut source_revisions = BTreeMap::new();
    for id in &selected_ids {
        let path = index::note_path(vault, id)?;
        let markdown = fs::read_to_string(&path).map_err(|error| MemoryError::io(&path, error))?;
        let note = Note::parse(&path, &markdown)?;
        if note.frontmatter.state == NoteState::Archived {
            return Err(MemoryError::Validation(format!(
                "archived note `{id}` cannot be promoted"
            )));
        }
        let source_revision = sha256_hex(markdown.as_bytes());
        source_revisions.insert(id.clone(), source_revision.clone());
        workflow_inputs.push(PromotionInput {
            source_id: id.clone(),
            source_revision,
            target_id: ulid::Ulid::generate().to_string(),
            title: note.frontmatter.title.clone(),
            body: note.body.clone(),
            sources: note.frontmatter.sources.clone(),
        });
    }
    let (nearby_notes, exact_duplicates, similarity_candidates, graph_neighborhood) =
        promotion_context(vault, &workflow_inputs)?;
    let workflow_args = WorkflowArgs {
        schema: PROMOTION_SCHEMA.into(),
        inputs: workflow_inputs,
        nearby_notes,
        exact_duplicates,
        similarity_candidates,
        graph_neighborhood,
        policy: serde_json::json!({
            "accepted_state": "accepted",
            "require_link_reasons": true,
            "preserve_sources": true,
            "mutation_allowed": false
        }),
    };
    let proposal = run_promotion_workflow(&workflow_source, &workflow_args)?;
    validate_workflow_proposal(&proposal, &workflow_args)?;
    let legacy_proposals = legacy_proposals(&proposal.actions);
    let stored_input = StoredPromotionInput {
        selected_ids,
        source_revisions,
        workflow_fingerprint,
        workflow_args,
    };
    let operation_key = sha256_hex(
        serde_json::to_vec(&serde_json::json!({
            "schema": PROMOTION_SCHEMA,
            "input": stored_input,
            "proposal": proposal,
        }))?
        .as_slice(),
    );
    let result = PromotionResult {
        operation_key: operation_key.clone(),
        applied: false,
        actions: proposal.actions.clone(),
        proposals: legacy_proposals,
        created: Vec::new(),
    };
    let _writer_lock = vault.lock_writer(true)?;
    let connection = index::open_writer(vault)?;
    recover_transactions_locked(vault, &connection)?;
    confirm_source_revisions(vault, &stored_input)?;
    connection.execute(
        "INSERT INTO promotion_runs(
             operation_key, input_json, proposal_json, result_json, state, created_at
         ) VALUES (?1, ?2, ?3, NULL, 'proposed', ?4)
         ON CONFLICT(operation_key) DO NOTHING",
        params![
            operation_key,
            serde_json::to_string(&stored_input)?,
            serde_json::to_string(&proposal)?,
            now
        ],
    )?;
    Ok(result)
}

#[allow(clippy::type_complexity)]
fn promotion_context(
    vault: &Vault,
    inputs: &[PromotionInput],
) -> Result<(
    Vec<NearbyNote>,
    Vec<DuplicateCandidate>,
    Vec<SimilarityCandidate>,
    Vec<WorkflowEdge>,
)> {
    let selected = inputs
        .iter()
        .map(|input| input.source_id.as_str())
        .collect::<BTreeSet<_>>();
    let mut nearby_notes = Vec::new();
    for summary in index::all_notes(vault)? {
        if selected.contains(summary.id.as_str())
            || summary.state != NoteState::Accepted.to_string()
            || nearby_notes.len() >= 32
        {
            continue;
        }
        let markdown = fs::read_to_string(&summary.path)
            .map_err(|error| MemoryError::io(&summary.path, error))?;
        let note = Note::parse(&summary.path, &markdown)?;
        nearby_notes.push(NearbyNote {
            id: summary.id,
            note_type: summary.note_type,
            title: summary.title,
            body: truncate_chars(&note.body, 16 * 1024),
            body_hash: sha256_hex(normalize_duplicate_body(&note.body).as_bytes()),
            revision: sha256_hex(markdown.as_bytes()),
        });
    }

    let mut exact_duplicates = Vec::new();
    let mut similarity_candidates = Vec::new();
    for input in inputs {
        let source_terms = lexical_terms(&input.body);
        for candidate in &nearby_notes {
            if sha256_hex(normalize_duplicate_body(&input.body).as_bytes()) == candidate.body_hash {
                exact_duplicates.push(DuplicateCandidate {
                    source_id: input.source_id.clone(),
                    candidate_id: candidate.id.clone(),
                });
                continue;
            }
            let candidate_terms = lexical_terms(&candidate.body);
            let shared_terms = source_terms
                .intersection(&candidate_terms)
                .take(12)
                .cloned()
                .collect::<Vec<_>>();
            if shared_terms.len() >= 2 {
                similarity_candidates.push(SimilarityCandidate {
                    source_id: input.source_id.clone(),
                    candidate_id: candidate.id.clone(),
                    shared_terms,
                });
            }
        }
    }
    similarity_candidates.sort_by(|left, right| {
        right.shared_terms.len().cmp(&left.shared_terms.len()).then(
            (&left.source_id, &left.candidate_id).cmp(&(&right.source_id, &right.candidate_id)),
        )
    });
    similarity_candidates.truncate(32);

    let context_ids = selected
        .iter()
        .copied()
        .chain(nearby_notes.iter().map(|note| note.id.as_str()))
        .collect::<BTreeSet<_>>();
    let graph_neighborhood = index::all_edges(vault)?
        .into_iter()
        .filter(|edge| {
            context_ids.contains(edge.from.as_str()) || context_ids.contains(edge.to.as_str())
        })
        .map(|edge| WorkflowEdge {
            from: edge.from,
            to: edge.to,
            relation: edge.relation,
            reason: edge.reason,
        })
        .collect();
    Ok((
        nearby_notes,
        exact_duplicates,
        similarity_candidates,
        graph_neighborhood,
    ))
}

fn normalize_duplicate_body(body: &str) -> String {
    body.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn lexical_terms(body: &str) -> BTreeSet<String> {
    body.split(|character: char| !character.is_alphanumeric() && character != '_')
        .filter(|term| term.chars().count() >= 3)
        .map(str::to_lowercase)
        .collect()
}

fn truncate_chars(
    value: &str,
    limit: usize,
) -> String {
    value.chars().take(limit).collect()
}

fn promotion_workflow(vault: &Vault) -> Result<(String, String)> {
    let project = vault
        .project_path
        .join(".ren")
        .join("workflows")
        .join("zettelkasten-promote.rhai");
    let user = std::env::var_os("HOME").map(PathBuf::from).map(|home| {
        home.join(".ren")
            .join("workflows")
            .join("zettelkasten-promote.rhai")
    });
    let source = if project.is_file() {
        fs::read_to_string(&project).map_err(|error| MemoryError::io(&project, error))?
    } else if let Some(user) = user.filter(|path| path.is_file()) {
        fs::read_to_string(&user).map_err(|error| MemoryError::io(&user, error))?
    } else {
        PROMOTION_WORKFLOW.into()
    };
    let fingerprint = sha256_hex(source.as_bytes());
    Ok((source, fingerprint))
}

fn run_promotion_workflow(
    source: &str,
    args: &WorkflowArgs,
) -> Result<WorkflowProposal> {
    let engine = ren_workflow::Engine::new(ren_workflow::EchoHost);
    let workflow = engine
        .compile(source)
        .map_err(|error| MemoryError::Workflow(error.to_string()))?;
    let args = serde_json::to_value(args)?;
    if let Some(schema) = &workflow.metadata().args_schema {
        ren_workflow::validate_args(schema, Some(&args))
            .map_err(|error| MemoryError::Workflow(error.to_string()))?;
    }
    let result = engine
        .run(
            &workflow,
            ren_workflow::RunOptions {
                args: Some(args),
                agent_budget: 1,
                ..ren_workflow::RunOptions::default()
            },
        )
        .map_err(|error| MemoryError::Workflow(error.to_string()))?;
    if result.journal.entries().iter().any(|entry| {
        matches!(
            entry,
            ren_workflow::JournalEntry::Agent { .. } | ren_workflow::JournalEntry::Parallel { .. }
        )
    }) {
        return Err(MemoryError::Workflow(
            "promotion workflows that call agent() or parallel() are not supported by `ren \
             memory promote`; use a deterministic proposal-only workflow"
                .into(),
        ));
    }
    let complete = result.complete.ok_or_else(|| {
        MemoryError::Workflow("zettelkasten-promote did not return a proposal".into())
    })?;
    decode_workflow_proposal(complete)
}

fn validate_workflow_proposal(
    proposal: &WorkflowProposal,
    args: &WorkflowArgs,
) -> Result<()> {
    if proposal.schema != ACTION_PROPOSAL_SCHEMA {
        return Err(MemoryError::Workflow(format!(
            "unsupported proposal schema `{}`",
            proposal.schema
        )));
    }
    if proposal.actions.is_empty() {
        return Err(MemoryError::Workflow(
            "promotion workflow returned no proposals".into(),
        ));
    }
    let inputs = args
        .inputs
        .iter()
        .map(|input| (input.source_id.as_str(), input))
        .collect::<BTreeMap<_, _>>();
    let mut target_ids = BTreeSet::new();
    for action in &proposal.actions {
        let sources = action_sources(action);
        validate_action_sources(sources, &inputs)?;
        match action {
            PromotionAction::KeepFleeting { rationale, .. }
            | PromotionAction::NeedsContext { rationale, .. }
            | PromotionAction::Archive { rationale, .. }
            | PromotionAction::Duplicate { rationale, .. } => {
                validate_rationale(rationale)?;
                if let PromotionAction::Duplicate { candidate_id, .. } = action {
                    crate::model::validate_id(candidate_id)?;
                }
            },
            PromotionAction::CreateNote {
                target_id,
                target_type,
                body,
                rationale,
                links,
                ..
            } => {
                validate_durable_action(
                    target_id,
                    target_type,
                    body,
                    rationale,
                    links,
                    &mut target_ids,
                    false,
                )?;
            },
            PromotionAction::UpsertCollection {
                target_id,
                target_revision,
                target_type,
                body,
                rationale,
                links,
                ..
            } => {
                if target_revision
                    .as_ref()
                    .is_some_and(|revision| !is_sha256(revision))
                {
                    return Err(MemoryError::Workflow(
                        "collection target_revision must be a SHA-256 hex digest".into(),
                    ));
                }
                validate_durable_action(
                    target_id,
                    target_type,
                    body,
                    rationale,
                    links,
                    &mut target_ids,
                    true,
                )?;
            },
            PromotionAction::SuggestLink {
                from_id,
                to_id,
                reason,
                ..
            } => {
                crate::model::validate_id(from_id)?;
                crate::model::validate_id(to_id)?;
                if reason.trim().is_empty() {
                    return Err(MemoryError::Workflow(
                        "suggested links require a reason".into(),
                    ));
                }
                if !sources.iter().any(|source| source.source_id == *from_id) {
                    return Err(MemoryError::Workflow(
                        "suggested-link source must include from_id and its revision".into(),
                    ));
                }
            },
        }
    }
    Ok(())
}

fn decode_workflow_proposal(value: serde_json::Value) -> Result<WorkflowProposal> {
    let schema = value
        .get("schema")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| MemoryError::Workflow("proposal output is missing `schema`".into()))?;
    if schema == ACTION_PROPOSAL_SCHEMA {
        return serde_json::from_value(value)
            .map_err(|error| MemoryError::Workflow(format!("invalid proposal output: {error}")));
    }
    if schema == PROPOSAL_SCHEMA {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct LegacyProposal {
            schema: String,
            proposals: Vec<PromotionProposal>,
        }
        let legacy: LegacyProposal = serde_json::from_value(value)
            .map_err(|error| MemoryError::Workflow(format!("invalid proposal output: {error}")))?;
        let _ = legacy.schema;
        let actions = legacy
            .proposals
            .into_iter()
            .map(|proposal| PromotionAction::CreateNote {
                sources: vec![PromotionSourceRevision {
                    source_id: proposal.source_id,
                    source_revision: proposal.source_revision,
                }],
                target_id: proposal.target_id,
                target_type: proposal.target_type,
                title: proposal.title,
                body: proposal.body,
                rationale: proposal.rationale,
                source_locators: proposal.sources,
                links: proposal.links,
            })
            .collect();
        return Ok(WorkflowProposal {
            schema: ACTION_PROPOSAL_SCHEMA.into(),
            actions,
        });
    }
    Err(MemoryError::Workflow(format!(
        "unsupported proposal schema `{schema}`"
    )))
}

fn action_sources(action: &PromotionAction) -> &[PromotionSourceRevision] {
    match action {
        PromotionAction::KeepFleeting { sources, .. }
        | PromotionAction::NeedsContext { sources, .. }
        | PromotionAction::Archive { sources, .. }
        | PromotionAction::CreateNote { sources, .. }
        | PromotionAction::UpsertCollection { sources, .. }
        | PromotionAction::SuggestLink { sources, .. }
        | PromotionAction::Duplicate { sources, .. } => sources,
    }
}

fn validate_action_sources(
    sources: &[PromotionSourceRevision],
    inputs: &BTreeMap<&str, &PromotionInput>,
) -> Result<()> {
    if sources.is_empty() {
        return Err(MemoryError::Workflow(
            "promotion actions require at least one source revision".into(),
        ));
    }
    let mut seen = BTreeSet::new();
    for source in sources {
        if !seen.insert(&source.source_id) {
            return Err(MemoryError::Workflow(format!(
                "promotion action repeats source `{}`",
                source.source_id
            )));
        }
        let input = inputs.get(source.source_id.as_str()).ok_or_else(|| {
            MemoryError::Workflow(format!(
                "proposal references unselected source `{}`",
                source.source_id
            ))
        })?;
        if source.source_revision != input.source_revision {
            return Err(MemoryError::Workflow(format!(
                "proposal revision for `{}` does not match the workflow input",
                source.source_id
            )));
        }
    }
    Ok(())
}

fn validate_rationale(rationale: &str) -> Result<()> {
    if rationale.trim().is_empty() {
        Err(MemoryError::Workflow(
            "promotion actions require a rationale".into(),
        ))
    } else {
        Ok(())
    }
}

fn validate_durable_action(
    target_id: &str,
    target_type: &str,
    body: &str,
    rationale: &str,
    links: &[Link],
    target_ids: &mut BTreeSet<String>,
    collection_only: bool,
) -> Result<()> {
    crate::model::validate_id(target_id)?;
    if !target_ids.insert(target_id.into()) {
        return Err(MemoryError::Workflow(format!(
            "duplicate proposal target `{target_id}`"
        )));
    }
    let target_type = target_type.parse::<NoteType>()?;
    if target_type == NoteType::Fleeting
        || (collection_only && !matches!(target_type, NoteType::Structure | NoteType::Index))
    {
        return Err(MemoryError::Workflow(
            "promotion target type is invalid for this action".into(),
        ));
    }
    if body.trim().is_empty() {
        return Err(MemoryError::Workflow(
            "durable proposals require a body".into(),
        ));
    }
    validate_rationale(rationale)?;
    for link in links {
        crate::model::validate_id(&link.to)?;
        if link
            .reason
            .as_ref()
            .is_none_or(|reason| reason.trim().is_empty())
        {
            return Err(MemoryError::Workflow(
                "proposal links require reasons".into(),
            ));
        }
    }
    Ok(())
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn legacy_proposals(actions: &[PromotionAction]) -> Vec<PromotionProposal> {
    actions
        .iter()
        .filter_map(|action| {
            let PromotionAction::CreateNote {
                sources,
                target_id,
                target_type,
                title,
                body,
                rationale,
                source_locators,
                links,
            } = action
            else {
                return None;
            };
            let source = sources.first()?;
            Some(PromotionProposal {
                source_id: source.source_id.clone(),
                source_revision: source.source_revision.clone(),
                target_id: target_id.clone(),
                target_type: target_type.clone(),
                title: title.clone(),
                body: body.clone(),
                rationale: rationale.clone(),
                sources: source_locators.clone(),
                links: links.clone(),
            })
        })
        .collect()
}

fn source_note_mut<'a>(
    sources: &'a mut BTreeMap<String, (PathBuf, String, Note)>,
    id: &str,
) -> Result<&'a mut (PathBuf, String, Note)> {
    sources
        .get_mut(id)
        .ok_or_else(|| MemoryError::Workflow(format!("missing source `{id}`")))
}

#[allow(clippy::too_many_arguments)]
fn new_promoted_note(
    sources: &BTreeMap<String, (PathBuf, String, Note)>,
    action_sources: &[PromotionSourceRevision],
    target_id: &str,
    target_type: NoteType,
    title: Option<String>,
    body: String,
    source_locators: &[crate::model::Source],
    links: &[Link],
    now: &str,
) -> Result<Note> {
    let first = action_sources
        .first()
        .and_then(|source| sources.get(&source.source_id))
        .ok_or_else(|| MemoryError::Workflow("promotion action has no loaded source".into()))?;
    let mut target = Note::new(target_type, NoteState::Accepted, now.into(), title, body);
    target.frontmatter.id = target_id.into();
    target
        .frontmatter
        .project
        .clone_from(&first.2.frontmatter.project);
    target
        .frontmatter
        .tags
        .clone_from(&first.2.frontmatter.tags);
    target.frontmatter.sources = source_locators.to_vec();
    target.frontmatter.links = links.to_vec();
    target.frontmatter.promoted_from = action_sources
        .iter()
        .map(|source| source.source_id.clone())
        .collect();
    Ok(target)
}

fn add_origin_links(
    target: &mut Note,
    action_sources: &[PromotionSourceRevision],
) {
    for source in action_sources {
        if !target
            .frontmatter
            .links
            .iter()
            .any(|link| link.to == source.source_id && link.rel == Relation::SourceOf)
        {
            target.frontmatter.links.push(Link {
                to: source.source_id.clone(),
                rel: Relation::SourceOf,
                reason: Some("Preserves an originating note and reviewed revision.".into()),
            });
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn link_sources_to_target(
    sources: &mut BTreeMap<String, (PathBuf, String, Note)>,
    action_sources: &[PromotionSourceRevision],
    target_id: &str,
    relation: Relation,
    operation_key: &str,
    now: &str,
    modified_sources: &mut BTreeSet<String>,
) -> Result<()> {
    for source_ref in action_sources {
        let (_, _, source) = source_note_mut(sources, &source_ref.source_id)?;
        source.frontmatter.links.push(Link {
            to: target_id.into(),
            rel: relation,
            reason: Some(format!(
                "Accepted from reviewed promotion operation {operation_key}."
            )),
        });
        source.frontmatter.updated_at = Some(now.into());
        source.frontmatter.validate()?;
        modified_sources.insert(source_ref.source_id.clone());
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn apply_promotion(
    vault: &Vault,
    ids: &[String],
    requested_operation: Option<&str>,
) -> Result<PromotionResult> {
    let writer_lock = vault.lock_writer(true)?;
    let mut connection = index::open_writer(vault)?;
    recover_transactions_locked(vault, &connection)?;
    let run = load_stored_run(&connection, ids, requested_operation)?;
    if run.state == "applied" {
        return run.result.ok_or_else(|| {
            MemoryError::Validation(format!(
                "applied promotion `{}` has no recorded result",
                run.operation_key
            ))
        });
    }
    if run.state != "proposed" {
        return Err(MemoryError::Validation(format!(
            "promotion `{}` is in unsupported state `{}`",
            run.operation_key, run.state
        )));
    }
    confirm_source_revisions(vault, &run.input)?;
    validate_workflow_proposal(&run.proposal, &run.input.workflow_args)?;

    let mut sources = BTreeMap::<String, (PathBuf, String, Note)>::new();
    for input in &run.input.workflow_args.inputs {
        let path = index::note_path(vault, &input.source_id)?;
        let markdown = fs::read_to_string(&path).map_err(|error| MemoryError::io(&path, error))?;
        let note = Note::parse(&path, &markdown)?;
        sources.insert(input.source_id.clone(), (path, markdown, note));
    }
    let mut changes = Vec::new();
    let mut created = Vec::new();
    let mut modified_sources = BTreeSet::new();
    let mut archived_sources = BTreeSet::new();
    let mut updated_collections = BTreeMap::<String, (PathBuf, String, Note)>::new();
    let now = jiff::Timestamp::now().to_string();
    let compatibility_proposals = legacy_proposals(&run.proposal.actions);
    for action in &run.proposal.actions {
        match action {
            PromotionAction::KeepFleeting { .. } | PromotionAction::Duplicate { .. } => {},
            PromotionAction::NeedsContext {
                sources: action_sources,
                ..
            } => {
                for source_ref in action_sources {
                    let (_, _, source) = source_note_mut(&mut sources, &source_ref.source_id)?;
                    source.frontmatter.state = NoteState::NeedsContext;
                    source.frontmatter.updated_at = Some(now.clone());
                    modified_sources.insert(source_ref.source_id.clone());
                }
            },
            PromotionAction::Archive {
                sources: action_sources,
                ..
            } => {
                for source_ref in action_sources {
                    let (_, _, source) = source_note_mut(&mut sources, &source_ref.source_id)?;
                    source.frontmatter.state = NoteState::Archived;
                    source.frontmatter.updated_at = Some(now.clone());
                    modified_sources.insert(source_ref.source_id.clone());
                    archived_sources.insert(source_ref.source_id.clone());
                }
            },
            PromotionAction::CreateNote {
                sources: action_sources,
                target_id,
                target_type,
                title,
                body,
                source_locators,
                links,
                ..
            } => {
                let target_type = target_type.parse::<NoteType>()?;
                let mut target = new_promoted_note(
                    &sources,
                    action_sources,
                    target_id,
                    target_type,
                    title.clone(),
                    body.clone(),
                    source_locators,
                    links,
                    &now,
                )?;
                add_origin_links(&mut target, action_sources);
                target.frontmatter.validate()?;
                let destination = vault.safe_note_path(target_type.directory(), target_id)?;
                if destination.exists() {
                    return Err(MemoryError::Validation(format!(
                        "promotion target already exists: {}",
                        destination.display()
                    )));
                }
                changes.push(FileChange {
                    target: destination.clone(),
                    original: None,
                    replacement: Some(target.to_markdown()?.into_bytes()),
                });
                link_sources_to_target(
                    &mut sources,
                    action_sources,
                    target_id,
                    Relation::PromotedTo,
                    &run.operation_key,
                    &now,
                    &mut modified_sources,
                )?;
                created.push(MutationResult {
                    action: "promoted".into(),
                    note_id: target_id.clone(),
                    path: destination,
                });
            },
            PromotionAction::UpsertCollection {
                sources: action_sources,
                target_id,
                target_revision,
                target_type,
                title,
                body,
                source_locators,
                links,
                ..
            } => {
                let target_type = target_type.parse::<NoteType>()?;
                let destination = vault.safe_note_path(target_type.directory(), target_id)?;
                if let Some(expected_revision) = target_revision {
                    let indexed_path = index::note_path(vault, target_id)?;
                    if indexed_path != destination {
                        return Err(MemoryError::Validation(format!(
                            "collection `{target_id}` is not stored as {target_type}"
                        )));
                    }
                    let original = fs::read_to_string(&indexed_path)
                        .map_err(|error| MemoryError::io(&indexed_path, error))?;
                    if sha256_hex(original.as_bytes()) != *expected_revision {
                        return Err(MemoryError::Validation(format!(
                            "stale collection update for `{target_id}`"
                        )));
                    }
                    let mut target = Note::parse(&indexed_path, &original)?;
                    target.frontmatter.title.clone_from(title);
                    target.frontmatter.updated_at = Some(now.clone());
                    target.body.clone_from(body);
                    target.frontmatter.sources.extend(source_locators.clone());
                    target.frontmatter.links.extend(links.clone());
                    target
                        .frontmatter
                        .promoted_from
                        .extend(action_sources.iter().map(|source| source.source_id.clone()));
                    target.frontmatter.validate()?;
                    updated_collections
                        .insert(target_id.clone(), (indexed_path.clone(), original, target));
                    created.push(MutationResult {
                        action: "updated".into(),
                        note_id: target_id.clone(),
                        path: indexed_path,
                    });
                } else {
                    if destination.exists() {
                        return Err(MemoryError::Validation(format!(
                            "collection target already exists without target_revision: {}",
                            destination.display()
                        )));
                    }
                    let mut target = new_promoted_note(
                        &sources,
                        action_sources,
                        target_id,
                        target_type,
                        title.clone(),
                        body.clone(),
                        source_locators,
                        links,
                        &now,
                    )?;
                    add_origin_links(&mut target, action_sources);
                    target.frontmatter.validate()?;
                    changes.push(FileChange {
                        target: destination.clone(),
                        original: None,
                        replacement: Some(target.to_markdown()?.into_bytes()),
                    });
                    created.push(MutationResult {
                        action: "created".into(),
                        note_id: target_id.clone(),
                        path: destination,
                    });
                }
                let relation = if target_type == NoteType::Structure {
                    Relation::MemberOfStructure
                } else {
                    Relation::Related
                };
                link_sources_to_target(
                    &mut sources,
                    action_sources,
                    target_id,
                    relation,
                    &run.operation_key,
                    &now,
                    &mut modified_sources,
                )?;
            },
            PromotionAction::SuggestLink {
                sources: action_sources,
                from_id,
                to_id,
                relation,
                reason,
            } => {
                let (_, _, source) = source_note_mut(&mut sources, from_id)?;
                source.frontmatter.links.push(Link {
                    to: to_id.clone(),
                    rel: *relation,
                    reason: Some(reason.clone()),
                });
                source.frontmatter.updated_at = Some(now.clone());
                source.frontmatter.validate()?;
                if !action_sources
                    .iter()
                    .any(|source| source.source_id == *from_id)
                {
                    return Err(MemoryError::Workflow(
                        "suggested link does not carry the modified source revision".into(),
                    ));
                }
                modified_sources.insert(from_id.clone());
            },
        }
    }
    for (id, (path, original, source)) in sources {
        if !modified_sources.contains(&id) {
            continue;
        }
        source.frontmatter.clone().validate()?;
        if archived_sources.contains(&id) {
            let destination = vault.safe_note_path("archived", &id)?;
            if destination != path && destination.exists() {
                return Err(MemoryError::Validation(format!(
                    "archive destination already exists: {}",
                    destination.display()
                )));
            }
            changes.push(FileChange {
                target: destination,
                original: None,
                replacement: Some(source.to_markdown()?.into_bytes()),
            });
            changes.push(FileChange {
                target: path,
                original: Some(original.as_bytes().to_vec()),
                replacement: None,
            });
        } else {
            changes.push(FileChange {
                target: path,
                original: Some(original.as_bytes().to_vec()),
                replacement: Some(source.to_markdown()?.into_bytes()),
            });
        }
        changes.push(revision_change(vault, &id, &original)?);
    }
    for (id, (path, original, target)) in updated_collections {
        changes.push(FileChange {
            target: path,
            original: Some(original.as_bytes().to_vec()),
            replacement: Some(target.to_markdown()?.into_bytes()),
        });
        changes.push(revision_change(vault, &id, &original)?);
    }

    let mut filesystem = PreparedFileTransaction::prepare(vault, &run.operation_key, changes)?;
    filesystem.apply()?;
    let result = PromotionResult {
        operation_key: run.operation_key.clone(),
        applied: true,
        actions: run.proposal.actions,
        proposals: compatibility_proposals,
        created,
    };
    let database_result = (|| {
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let updated = transaction.execute(
            "UPDATE promotion_runs
             SET result_json = ?2, state = 'applied'
             WHERE operation_key = ?1 AND state = 'proposed'",
            params![run.operation_key, serde_json::to_string(&result)?],
        )?;
        if updated != 1 {
            return Err(MemoryError::Validation(
                "promotion state changed before apply commit".into(),
            ));
        }
        transaction.commit()?;
        Ok(())
    })();
    if let Err(error) = database_result {
        filesystem.rollback()?;
        return Err(error);
    }
    filesystem.commit()?;
    drop(connection);
    drop(writer_lock);
    index::sync(vault, false, true)?;
    Ok(result)
}

fn load_stored_run(
    connection: &Connection,
    ids: &[String],
    requested_operation: Option<&str>,
) -> Result<StoredRun> {
    if let Some(operation_key) = requested_operation {
        let run = connection
            .query_row(
                "SELECT operation_key, input_json, proposal_json, result_json, state
                 FROM promotion_runs WHERE operation_key = ?1",
                [operation_key],
                stored_run_from_row,
            )
            .optional()?
            .ok_or_else(|| {
                MemoryError::Validation(format!(
                    "promotion operation `{operation_key}` was not found"
                ))
            })?;
        if !ids.is_empty() {
            let mut selected = ids.to_vec();
            selected.sort();
            selected.dedup();
            if run.input.selected_ids != selected {
                return Err(MemoryError::Validation(format!(
                    "promotion operation `{operation_key}` does not match the supplied note ids"
                )));
            }
        }
        return Ok(run);
    }
    let mut selected = ids.to_vec();
    selected.sort();
    selected.dedup();
    let mut statement = connection.prepare(
        "SELECT operation_key, input_json, proposal_json, result_json, state
         FROM promotion_runs ORDER BY rowid DESC",
    )?;
    let rows = statement.query_map([], stored_run_from_row)?;
    for row in rows {
        let run = row?;
        if run.input.selected_ids == selected {
            return Ok(run);
        }
    }
    Err(MemoryError::Validation(
        "no reviewed promotion proposal matches these note ids; run promote without --apply first"
            .into(),
    ))
}

fn stored_run_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredRun> {
    let input_json: String = row.get(1)?;
    let proposal_json: String = row.get(2)?;
    let result_json: Option<String> = row.get(3)?;
    let parse = |column, error: serde_json::Error| {
        rusqlite::Error::FromSqlConversionFailure(
            column,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    };
    Ok(StoredRun {
        operation_key: row.get(0)?,
        input: serde_json::from_str(&input_json).map_err(|error| parse(1, error))?,
        proposal: serde_json::from_str(&proposal_json).map_err(|error| parse(2, error))?,
        result: result_json
            .map(|value| serde_json::from_str(&value).map_err(|error| parse(3, error)))
            .transpose()?,
        state: row.get(4)?,
    })
}

fn confirm_source_revisions(
    vault: &Vault,
    input: &StoredPromotionInput,
) -> Result<()> {
    for (id, expected) in &input.source_revisions {
        let path = index::note_path(vault, id)?;
        let current = fs::read_to_string(&path).map_err(|error| MemoryError::io(&path, error))?;
        let actual = sha256_hex(current.as_bytes());
        if actual != *expected {
            return Err(MemoryError::Validation(format!(
                "stale promotion proposal for `{id}`: source revision changed"
            )));
        }
    }
    Ok(())
}

#[derive(Clone, Debug)]
struct FileChange {
    target: PathBuf,
    original: Option<Vec<u8>>,
    replacement: Option<Vec<u8>>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct TransactionManifest {
    schema: String,
    operation_key: String,
    state: String,
    files: Vec<ManifestFile>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ManifestFile {
    target: String,
    original: bool,
    replacement: bool,
    ordinal: usize,
}

struct PreparedFileTransaction {
    root: PathBuf,
    manifest: TransactionManifest,
    vault_root: PathBuf,
}

impl PreparedFileTransaction {
    fn prepare(
        vault: &Vault,
        operation_key: &str,
        changes: Vec<FileChange>,
    ) -> Result<Self> {
        if operation_key.len() != 64 || !operation_key.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(MemoryError::Validation(
                "filesystem transaction key is invalid".into(),
            ));
        }
        let root = vault.index_dir().join("transactions").join(operation_key);
        if root.exists() {
            return Err(MemoryError::Validation(format!(
                "filesystem transaction `{operation_key}` already exists"
            )));
        }
        create_private_dir(&root)?;
        let prepared = (|| {
            let mut files = Vec::with_capacity(changes.len());
            for (ordinal, change) in changes.into_iter().enumerate() {
                let relative = change
                    .target
                    .strip_prefix(&vault.root)
                    .map_err(|_| {
                        MemoryError::UnsafeInput("transaction target escapes the vault".into())
                    })?
                    .to_str()
                    .ok_or_else(|| {
                        MemoryError::UnsafeInput("transaction path is not valid UTF-8".into())
                    })?
                    .to_owned();
                validate_transaction_target(&vault.root, &relative)?;
                let actual = read_optional(&change.target)?;
                if actual != change.original {
                    return Err(MemoryError::Validation(format!(
                        "transaction target changed before apply: {}",
                        change.target.display()
                    )));
                }
                if let Some(original) = &change.original {
                    write_atomic_replace(&root.join(format!("{ordinal}.original")), original)?;
                }
                if let Some(replacement) = &change.replacement {
                    write_atomic_replace(
                        &root.join(format!("{ordinal}.replacement")),
                        replacement,
                    )?;
                }
                files.push(ManifestFile {
                    target: relative,
                    original: change.original.is_some(),
                    replacement: change.replacement.is_some(),
                    ordinal,
                });
            }
            let manifest = TransactionManifest {
                schema: "ren-memory-filesystem-transaction/v1".into(),
                operation_key: operation_key.into(),
                state: "prepared".into(),
                files,
            };
            write_manifest(&root, &manifest)?;
            Ok(manifest)
        })();
        let manifest = match prepared {
            Ok(manifest) => manifest,
            Err(error) => {
                let _ = remove_transaction_dir(&root);
                return Err(error);
            },
        };
        Ok(Self {
            root,
            manifest,
            vault_root: vault.root.clone(),
        })
    }

    fn apply(&mut self) -> Result<()> {
        self.manifest.state = "committing".into();
        write_manifest(&self.root, &self.manifest)?;
        if let Err(error) = roll_files(&self.vault_root, &self.root, &self.manifest.files, true) {
            self.rollback()?;
            return Err(error);
        }
        Ok(())
    }

    fn rollback(&mut self) -> Result<()> {
        roll_files(&self.vault_root, &self.root, &self.manifest.files, false)?;
        self.manifest.state = "rolled_back".into();
        write_manifest(&self.root, &self.manifest)?;
        remove_transaction_dir(&self.root)
    }

    fn commit(mut self) -> Result<()> {
        self.manifest.state = "committed".into();
        write_manifest(&self.root, &self.manifest)?;
        remove_transaction_dir(&self.root)
    }
}

pub fn recover_transactions_locked(
    vault: &Vault,
    connection: &Connection,
) -> Result<()> {
    let root = vault.index_dir().join("transactions");
    if !root.exists() {
        return Ok(());
    }
    let mut entries = fs::read_dir(&root)
        .map_err(|error| MemoryError::io(&root, error))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|error| MemoryError::io(&root, error))?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        if !entry
            .file_type()
            .map_err(|error| MemoryError::io(&path, error))?
            .is_dir()
        {
            continue;
        }
        let manifest_path = path.join("manifest.json");
        if !manifest_path.exists() {
            remove_transaction_dir(&path)?;
            continue;
        }
        let input = fs::read_to_string(&manifest_path)
            .map_err(|error| MemoryError::io(&manifest_path, error))?;
        let manifest: TransactionManifest = serde_json::from_str(&input)?;
        validate_recovery_manifest(vault, &path, &manifest)?;
        let database_applied = connection
            .query_row(
                "SELECT state = 'applied' FROM promotion_runs WHERE operation_key = ?1",
                [&manifest.operation_key],
                |row| row.get::<_, bool>(0),
            )
            .optional()?
            .unwrap_or(false);
        let roll_forward = manifest.state == "committed" || database_applied;
        roll_files(&vault.root, &path, &manifest.files, roll_forward)?;
        remove_transaction_dir(&path)?;
    }
    Ok(())
}

fn roll_files(
    vault_root: &Path,
    transaction_root: &Path,
    files: &[ManifestFile],
    forward: bool,
) -> Result<()> {
    let iterator: Box<dyn Iterator<Item = &ManifestFile>> = if forward {
        Box::new(files.iter())
    } else {
        Box::new(files.iter().rev())
    };
    for file in iterator {
        let target = validate_transaction_target(vault_root, &file.target)?;
        let original = if file.original {
            Some(
                fs::read(transaction_root.join(format!("{}.original", file.ordinal)))
                    .map_err(|error| MemoryError::io(transaction_root, error))?,
            )
        } else {
            None
        };
        let replacement = if file.replacement {
            Some(
                fs::read(transaction_root.join(format!("{}.replacement", file.ordinal)))
                    .map_err(|error| MemoryError::io(transaction_root, error))?,
            )
        } else {
            None
        };
        if forward {
            roll_file_forward(&target, original.as_deref(), replacement.as_deref())?;
        } else {
            roll_file_back(&target, original.as_deref(), replacement.as_deref())?;
        }
    }
    Ok(())
}

fn roll_file_forward(
    target: &Path,
    original: Option<&[u8]>,
    replacement: Option<&[u8]>,
) -> Result<()> {
    let current = read_optional(target)?;
    match (original, replacement, current.as_deref()) {
        (None, Some(replacement), None) => publish_new(target, replacement),
        (None, Some(replacement), Some(current)) if current == replacement => Ok(()),
        (Some(original), Some(replacement), Some(current)) if current == original => {
            write_atomic_replace(target, replacement)
        },
        (Some(_), Some(replacement), Some(current)) if current == replacement => Ok(()),
        (Some(original), None, Some(current)) if current == original => {
            fs::remove_file(target).map_err(|error| MemoryError::io(target, error))
        },
        (Some(_), None, None) => Ok(()),
        _ => Err(MemoryError::Validation(format!(
            "transaction target changed concurrently: {}",
            target.display()
        ))),
    }
}

fn roll_file_back(
    target: &Path,
    original: Option<&[u8]>,
    replacement: Option<&[u8]>,
) -> Result<()> {
    let current = read_optional(target)?;
    match (original, replacement, current.as_deref()) {
        (None, Some(_), None) => Ok(()),
        (None, Some(replacement), Some(current)) if current == replacement => {
            fs::remove_file(target).map_err(|error| MemoryError::io(target, error))
        },
        (Some(original), Some(_), Some(current)) if current == original => Ok(()),
        (Some(original), Some(replacement), Some(current)) if current == replacement => {
            write_atomic_replace(target, original)
        },
        (Some(original), None, None) => write_atomic_replace(target, original),
        (Some(original), None, Some(current)) if current == original => Ok(()),
        _ => Err(MemoryError::Validation(format!(
            "transaction target changed before rollback: {}",
            target.display()
        ))),
    }
}

fn read_optional(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(MemoryError::io(path, error)),
    }
}

fn validate_recovery_manifest(
    vault: &Vault,
    transaction_root: &Path,
    manifest: &TransactionManifest,
) -> Result<()> {
    if manifest.schema != "ren-memory-filesystem-transaction/v1" {
        return Err(MemoryError::Validation(format!(
            "unsupported recovery manifest in {}",
            transaction_root.display()
        )));
    }
    let directory_key = transaction_root
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| MemoryError::UnsafeInput("transaction directory has no name".into()))?;
    if manifest.operation_key != directory_key
        || manifest.operation_key.len() != 64
        || !manifest
            .operation_key
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(MemoryError::UnsafeInput(format!(
            "recovery manifest operation key does not match {}",
            transaction_root.display()
        )));
    }
    if !matches!(
        manifest.state.as_str(),
        "prepared" | "committing" | "committed" | "rolled_back"
    ) {
        return Err(MemoryError::Validation(format!(
            "unsupported transaction state `{}`",
            manifest.state
        )));
    }
    for (expected_ordinal, file) in manifest.files.iter().enumerate() {
        if file.ordinal != expected_ordinal || (!file.original && !file.replacement) {
            return Err(MemoryError::Validation(format!(
                "invalid transaction file entry {expected_ordinal}"
            )));
        }
        validate_transaction_target(&vault.root, &file.target)?;
        for (present, suffix) in [
            (file.original, "original"),
            (file.replacement, "replacement"),
        ] {
            if !present {
                continue;
            }
            let artifact = transaction_root.join(format!("{}.{}", file.ordinal, suffix));
            let metadata = fs::symlink_metadata(&artifact)
                .map_err(|error| MemoryError::io(&artifact, error))?;
            if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
                return Err(MemoryError::UnsafeInput(format!(
                    "transaction artifact is not a regular file: {}",
                    artifact.display()
                )));
            }
            let canonical =
                fs::canonicalize(&artifact).map_err(|error| MemoryError::io(&artifact, error))?;
            if !canonical.starts_with(transaction_root) {
                return Err(MemoryError::UnsafeInput(
                    "transaction artifact escapes its transaction directory".into(),
                ));
            }
        }
    }
    Ok(())
}

fn validate_transaction_target(
    vault_root: &Path,
    relative: &str,
) -> Result<PathBuf> {
    let relative_path = Path::new(relative);
    if relative_path.as_os_str().is_empty()
        || relative_path.is_absolute()
        || relative_path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(MemoryError::UnsafeInput(format!(
            "transaction target must be a normalized relative path: `{relative}`"
        )));
    }
    let first = relative_path
        .components()
        .next()
        .and_then(|component| match component {
            Component::Normal(value) => value.to_str(),
            _ => None,
        })
        .ok_or_else(|| MemoryError::UnsafeInput("transaction target is invalid".into()))?;
    if !matches!(
        first,
        "fleeting" | "literature" | "permanent" | "structure" | "index" | "archived" | ".revisions"
    ) {
        return Err(MemoryError::UnsafeInput(format!(
            "transaction target is outside managed note storage: `{relative}`"
        )));
    }
    let target = vault_root.join(relative_path);
    let parent = target
        .parent()
        .ok_or_else(|| MemoryError::UnsafeInput("transaction target has no parent".into()))?;
    let canonical_parent =
        fs::canonicalize(parent).map_err(|error| MemoryError::io(parent, error))?;
    let canonical_vault =
        fs::canonicalize(vault_root).map_err(|error| MemoryError::io(vault_root, error))?;
    if !canonical_parent.starts_with(&canonical_vault) {
        return Err(MemoryError::UnsafeInput(format!(
            "transaction target escapes the vault: `{relative}`"
        )));
    }
    match fs::symlink_metadata(&target) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(MemoryError::UnsafeInput(format!(
                "transaction target must not be a symlink: `{relative}`"
            )));
        },
        Ok(_) => {
            let canonical =
                fs::canonicalize(&target).map_err(|error| MemoryError::io(&target, error))?;
            if !canonical.starts_with(&canonical_vault) {
                return Err(MemoryError::UnsafeInput(format!(
                    "transaction target escapes the vault: `{relative}`"
                )));
            }
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
        Err(error) => return Err(MemoryError::io(&target, error)),
    }
    Ok(target)
}

fn write_manifest(
    root: &Path,
    manifest: &TransactionManifest,
) -> Result<()> {
    write_atomic_replace(
        &root.join("manifest.json"),
        &serde_json::to_vec_pretty(manifest)?,
    )
}

fn remove_transaction_dir(path: &Path) -> Result<()> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| MemoryError::UnsafeInput("transaction directory has no name".into()))?;
    if name.len() != 64 || !name.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(MemoryError::UnsafeInput(format!(
            "refusing to remove unexpected transaction directory {}",
            path.display()
        )));
    }
    fs::remove_dir_all(path).map_err(|error| MemoryError::io(path, error))
}

fn replacement_change(
    _vault: &Vault,
    path: &Path,
    original: &str,
    replacement: String,
) -> Result<FileChange> {
    if replacement.len() > crate::model::MAX_NOTE_BYTES {
        return Err(MemoryError::InputTooLarge {
            limit: crate::model::MAX_NOTE_BYTES,
        });
    }
    Ok(FileChange {
        target: path.to_owned(),
        original: Some(original.as_bytes().to_vec()),
        replacement: Some(replacement.into_bytes()),
    })
}

fn apply_note_replacements(
    vault: &Vault,
    operation_key: &str,
    mut changes: Vec<FileChange>,
) -> Result<()> {
    let mut revisions = Vec::new();
    for change in &changes {
        let Some(original) = &change.original else {
            continue;
        };
        let id = change
            .target
            .file_stem()
            .and_then(|stem| stem.to_str())
            .ok_or_else(|| MemoryError::UnsafeInput("note path has no UTF-8 id".into()))?;
        let markdown = std::str::from_utf8(original)
            .map_err(|_| MemoryError::Validation("note revision is not UTF-8".into()))?;
        revisions.push(revision_change(vault, id, markdown)?);
    }
    changes.extend(revisions);
    apply_file_transaction(vault, operation_key, changes)
}

fn apply_file_transaction(
    vault: &Vault,
    operation_key: &str,
    changes: Vec<FileChange>,
) -> Result<()> {
    let mut transaction = PreparedFileTransaction::prepare(vault, operation_key, changes)?;
    transaction.apply()?;
    transaction.commit()
}

fn revision_change(
    vault: &Vault,
    id: &str,
    markdown: &str,
) -> Result<FileChange> {
    crate::model::validate_id(id)?;
    let revision = sha256_hex(markdown.as_bytes());
    let directory = vault.root.join(".revisions").join(id);
    create_private_dir(&directory)?;
    let canonical =
        fs::canonicalize(&directory).map_err(|error| MemoryError::io(&directory, error))?;
    if !canonical.starts_with(&vault.root) {
        return Err(MemoryError::UnsafeInput(
            "revision path escapes the vault".into(),
        ));
    }
    let target = directory.join(format!("{revision}.md"));
    let original = if target.exists() {
        let existing = fs::read(&target).map_err(|error| MemoryError::io(&target, error))?;
        if existing != markdown.as_bytes() {
            return Err(MemoryError::Validation(format!(
                "immutable revision content is corrupt: {}",
                target.display()
            )));
        }
        Some(existing)
    } else {
        None
    };
    Ok(FileChange {
        target,
        original,
        replacement: Some(markdown.as_bytes().to_vec()),
    })
}

pub fn export_markdown(vault: &Vault) -> Result<String> {
    let notes = index::all_notes(vault)?;
    let mut output = String::new();
    for summary in notes {
        let note = read_note(&summary.path)?;
        append_format(
            &mut output,
            format_args!(
                "<!-- ren-memory:{} -->\n{}\n",
                summary.id,
                note.to_markdown()?
            ),
        )?;
    }
    Ok(output)
}

pub fn export_json(vault: &Vault) -> Result<serde_json::Value> {
    let summaries = index::all_notes(vault)?;
    let mut notes = Vec::new();
    for summary in summaries {
        let note = read_note(&summary.path)?;
        notes.push(serde_json::json!({
            "frontmatter": note.frontmatter,
            "body": note.body,
        }));
    }
    Ok(serde_json::json!({
        "schema": "ren-memory-export/v1",
        "vault": vault.id,
        "notes": notes,
        "edges": index::all_edges(vault)?,
    }))
}

pub fn export_dot(vault: &Vault) -> Result<String> {
    let notes = index::all_notes(vault)?;
    let edges = index::all_edges(vault)?;
    let mut output = String::from("digraph ren_memory {\n");
    for note in notes {
        let label = note.title.unwrap_or_else(|| note.id.clone());
        append_format(
            &mut output,
            format_args!(
                "  \"{}\" [label=\"{}\"];\n",
                dot_escape(&note.id),
                dot_escape(&label)
            ),
        )?;
    }
    for edge in edges {
        append_format(
            &mut output,
            format_args!(
                "  \"{}\" -> \"{}\" [label=\"{}\"];\n",
                dot_escape(&edge.from),
                dot_escape(&edge.to),
                dot_escape(&edge.relation)
            ),
        )?;
    }
    output.push_str("}\n");
    Ok(output)
}

fn sha256_hex(input: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut output = String::with_capacity(64);
    for byte in Sha256::digest(input) {
        if write!(&mut output, "{byte:02x}").is_err() {
            break;
        }
    }
    output
}

fn dot_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn append_format(
    output: &mut String,
    arguments: std::fmt::Arguments<'_>,
) -> Result<()> {
    output
        .write_fmt(arguments)
        .map_err(|error| MemoryError::Validation(format!("formatting export failed: {error}")))
}

#[cfg(test)]
mod transaction_tests {
    use std::fs;

    use super::{read_optional, roll_file_back, roll_file_forward, validate_transaction_target};

    #[test]
    fn rollback_handles_both_unapplied_and_applied_file_states() -> crate::error::Result<()> {
        let temporary = tempfile::tempdir().map_err(|error| crate::MemoryError::io(".", error))?;

        let modified = temporary.path().join("modified.md");
        fs::write(&modified, b"old").map_err(|error| crate::MemoryError::io(&modified, error))?;
        roll_file_back(&modified, Some(b"old"), Some(b"new"))?;
        assert_eq!(
            fs::read(&modified).map_err(|error| crate::MemoryError::io(&modified, error))?,
            b"old"
        );
        roll_file_forward(&modified, Some(b"old"), Some(b"new"))?;
        roll_file_back(&modified, Some(b"old"), Some(b"new"))?;
        assert_eq!(
            fs::read(&modified).map_err(|error| crate::MemoryError::io(&modified, error))?,
            b"old"
        );

        let created = temporary.path().join("created.md");
        roll_file_back(&created, None, Some(b"created"))?;
        roll_file_forward(&created, None, Some(b"created"))?;
        roll_file_back(&created, None, Some(b"created"))?;
        assert!(!created.exists());

        let deleted = temporary.path().join("deleted.md");
        fs::write(&deleted, b"existing")
            .map_err(|error| crate::MemoryError::io(&deleted, error))?;
        roll_file_back(&deleted, Some(b"existing"), None)?;
        roll_file_forward(&deleted, Some(b"existing"), None)?;
        roll_file_back(&deleted, Some(b"existing"), None)?;
        assert_eq!(
            fs::read(&deleted).map_err(|error| crate::MemoryError::io(&deleted, error))?,
            b"existing"
        );
        Ok(())
    }

    #[test]
    fn transaction_targets_reject_escape_and_symlink_paths() -> crate::error::Result<()> {
        let temporary = tempfile::tempdir().map_err(|error| crate::MemoryError::io(".", error))?;
        let root = temporary.path().join("vault");
        fs::create_dir_all(root.join("permanent"))
            .map_err(|error| crate::MemoryError::io(&root, error))?;
        assert!(validate_transaction_target(&root, "../outside").is_err());
        assert!(
            validate_transaction_target(
                &root,
                &temporary.path().join("absolute").to_string_lossy()
            )
            .is_err()
        );

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(temporary.path(), root.join("permanent/link.md"))
                .map_err(|error| crate::MemoryError::io(&root, error))?;
            assert!(validate_transaction_target(&root, "permanent/link.md").is_err());
        }
        Ok(())
    }

    #[test]
    fn optional_transaction_reads_only_suppress_not_found() -> crate::error::Result<()> {
        let temporary = tempfile::tempdir().map_err(|error| crate::MemoryError::io(".", error))?;
        assert!(read_optional(&temporary.path().join("missing"))?.is_none());
        assert!(read_optional(temporary.path()).is_err());
        Ok(())
    }
}

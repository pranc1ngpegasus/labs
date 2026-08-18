//! Durable journal with workflow/input/request content-hash identity.

use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

use crate::{AgentRequest, AgentResult, WorkflowError, hash::ContentHash};

/// The durable state of one slot in a parallel panel.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ParallelSlot {
    /// The panel was atomically admitted, but this slot has not committed yet.
    Pending,
    /// The slot committed; `None` represents an infrastructure-failed slot.
    Completed {
        /// Checksum of the committed result payload (`null` when `result` is `None`).
        result_hash: ContentHash,
        /// The committed agent result, or `None` for a failed slot.
        result: Option<AgentResult>,
    },
    /// The host returned an ambiguous failure; auto-retry is forbidden until
    /// [`Journal::retry_failed`] explicitly clears the slot.
    Ambiguous {
        /// Host-provided failure detail.
        message: String,
    },
}

/// One committed host call and its content-hash identity.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "call", rename_all = "snake_case")]
pub enum JournalEntry {
    /// A single agent invocation.
    Agent {
        /// Zero-based host-call sequence number.
        invocation: usize,
        /// Checksum of the canonical request payload.
        request_hash: ContentHash,
        /// Checksum of the canonical result payload.
        result_hash: ContentHash,
        /// Original request (kept for inspection; resume validates the hash).
        request: Box<AgentRequest>,
        /// Committed host result.
        result: Box<AgentResult>,
    },
    /// A serial agent call that failed ambiguously.
    ///
    /// Resume refuses to re-invoke the host until [`Journal::retry_failed`]
    /// truncates this entry.
    AgentAmbiguous {
        /// Zero-based host-call sequence number.
        invocation: usize,
        /// Checksum of the canonical request payload.
        request_hash: ContentHash,
        /// Original request.
        request: Box<AgentRequest>,
        /// Host-provided failure detail.
        message: String,
    },
    /// A barrier-style parallel panel.
    Parallel {
        /// Zero-based host-call sequence number.
        invocation: usize,
        /// Checksum of the ordered request panel.
        panel_hash: ContentHash,
        /// Original ordered requests.
        requests: Vec<AgentRequest>,
        /// Ordered durable slot states, one per request.
        slots: Vec<ParallelSlot>,
    },
    /// A scratch-file write.
    WriteScratch {
        /// Zero-based host-call sequence number.
        invocation: usize,
        /// Checksum of name + content.
        content_hash: ContentHash,
        /// Per-run scratch name.
        name: String,
        /// Written content.
        content: String,
    },
    /// A scratch-file read.
    ReadScratch {
        /// Zero-based host-call sequence number.
        invocation: usize,
        /// Checksum of name + observed content.
        content_hash: ContentHash,
        /// Per-run scratch name.
        name: String,
        /// Content observed by the read.
        content: String,
    },
    /// A user gate that has already paused once.
    AwaitUser {
        /// Zero-based host-call sequence number.
        invocation: usize,
        /// Checksum of kind + message.
        gate_hash: ContentHash,
        /// Gate kind.
        kind: String,
        /// Gate message.
        message: String,
    },
    /// A durable wake/timer armed until `due_ms`.
    ///
    /// The entry is written before the pause is acknowledged (output gate). The
    /// cursor advances only once `RunOptions::now_ms` covers `due_ms`.
    AwaitWake {
        /// Zero-based host-call sequence number.
        invocation: usize,
        /// Checksum of kind + `due_ms`.
        wake_hash: ContentHash,
        /// Wake kind (surfaced in [`crate::PauseInfo::kind`]).
        kind: String,
        /// Earliest wall-clock time (ms since epoch) that may consume this wake.
        due_ms: i64,
    },
}

impl JournalEntry {
    /// Returns the stable invocation number of this entry.
    #[must_use]
    pub const fn invocation(&self) -> usize {
        match self {
            Self::Agent { invocation, .. }
            | Self::AgentAmbiguous { invocation, .. }
            | Self::Parallel { invocation, .. }
            | Self::WriteScratch { invocation, .. }
            | Self::ReadScratch { invocation, .. }
            | Self::AwaitUser { invocation, .. }
            | Self::AwaitWake { invocation, .. } => *invocation,
        }
    }

    fn validate(
        &self,
        expected: usize,
    ) -> Result<(), WorkflowError> {
        if self.invocation() != expected {
            return Err(WorkflowError::JournalDivergence(format!(
                "entry {expected} has invocation {}",
                self.invocation()
            )));
        }
        match self {
            Self::Parallel {
                requests, slots, ..
            } if requests.len() != slots.len() => Err(WorkflowError::JournalDivergence(format!(
                "parallel entry {expected} has {} request(s) but {} slot state(s)",
                requests.len(),
                slots.len()
            ))),
            Self::Agent {
                request_hash,
                result_hash,
                request,
                result,
                ..
            } => validate_agent_hashes(expected, request_hash, result_hash, request, result),
            Self::AgentAmbiguous {
                request_hash,
                request,
                ..
            } => {
                let computed = request.content_hash()?;
                if &computed != request_hash {
                    return Err(WorkflowError::JournalDivergence(format!(
                        "agent_ambiguous entry {expected} request_hash does not match request payload"
                    )));
                }
                Ok(())
            },
            Self::Parallel {
                panel_hash,
                requests,
                slots,
                ..
            } => validate_parallel_hashes(expected, panel_hash, requests, slots),
            Self::WriteScratch {
                content_hash,
                name,
                content,
                ..
            } => validate_scratch_hashes(expected, "write_scratch", content_hash, name, content),
            Self::ReadScratch {
                content_hash,
                name,
                content,
                ..
            } => validate_scratch_hashes(expected, "read_scratch", content_hash, name, content),
            Self::AwaitUser {
                gate_hash,
                kind,
                message,
                ..
            } => {
                let computed = gate_content_hash(kind, message)?;
                if &computed != gate_hash {
                    return Err(WorkflowError::JournalDivergence(format!(
                        "await_user entry {expected} gate_hash does not match payload"
                    )));
                }
                Ok(())
            },
            Self::AwaitWake {
                wake_hash,
                kind,
                due_ms,
                ..
            } => {
                let computed = wake_content_hash(kind, *due_ms)?;
                if &computed != wake_hash {
                    return Err(WorkflowError::JournalDivergence(format!(
                        "await_wake entry {expected} wake_hash does not match payload"
                    )));
                }
                Ok(())
            },
        }
    }
}

/// Ordered committed host-call results for one workflow run.
///
/// Resume identity is the pair (`workflow_hash`, `input_hash`) plus each entry's
/// content hash. Changing the script, args, or any request invalidates replay.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Journal {
    /// SHA-256 of the workflow source script.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    workflow_hash: Option<ContentHash>,
    /// SHA-256 of the canonical workflow input (`args`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    input_hash: Option<ContentHash>,
    entries: Vec<JournalEntry>,
}

#[derive(Deserialize)]
struct RawJournal {
    #[serde(default)]
    workflow_hash: Option<ContentHash>,
    #[serde(default)]
    input_hash: Option<ContentHash>,
    entries: Vec<JournalEntry>,
}

impl<'de> Deserialize<'de> for Journal {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = RawJournal::deserialize(deserializer)?;
        Self::from_parts(raw.workflow_hash, raw.input_hash, raw.entries).map_err(D::Error::custom)
    }
}

impl Journal {
    /// Creates an empty journal with no bound identity yet.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            workflow_hash: None,
            input_hash: None,
            entries: Vec::new(),
        }
    }

    /// Creates a journal from identity hashes and entries, validating invariants.
    ///
    /// # Errors
    ///
    /// Returns [`WorkflowError::JournalDivergence`] when indices or checksums are invalid.
    pub fn from_parts(
        workflow_hash: Option<ContentHash>,
        input_hash: Option<ContentHash>,
        entries: Vec<JournalEntry>,
    ) -> Result<Self, WorkflowError> {
        let journal = Self {
            workflow_hash,
            input_hash,
            entries,
        };
        journal.validate()?;
        Ok(journal)
    }

    /// Validates invocation indices, identity pairing, and entry checksums.
    ///
    /// # Errors
    ///
    /// Returns [`WorkflowError::JournalDivergence`] on invariant failure.
    pub fn validate(&self) -> Result<(), WorkflowError> {
        match (&self.workflow_hash, &self.input_hash) {
            (None, None) | (Some(_), Some(_)) => {},
            _ => {
                return Err(WorkflowError::JournalDivergence(
                    "journal identity must bind workflow_hash and input_hash together".into(),
                ));
            },
        }
        if !self.entries.is_empty() && (self.workflow_hash.is_none() || self.input_hash.is_none()) {
            return Err(WorkflowError::JournalDivergence(
                "non-empty journal is missing workflow_hash or input_hash".into(),
            ));
        }
        for (expected, entry) in self.entries.iter().enumerate() {
            entry.validate(expected)?;
        }
        Ok(())
    }

    /// Binds identity on first use, or verifies checksums on resume.
    ///
    /// # Errors
    ///
    /// Returns [`WorkflowError::JournalDivergence`] when hashes do not match.
    pub fn bind_or_verify(
        &mut self,
        workflow_hash: ContentHash,
        input_hash: ContentHash,
    ) -> Result<(), WorkflowError> {
        match (&self.workflow_hash, &self.input_hash) {
            (None, None) if self.entries.is_empty() => {
                self.workflow_hash = Some(workflow_hash);
                self.input_hash = Some(input_hash);
                Ok(())
            },
            (Some(bound_workflow), Some(bound_input)) => {
                if bound_workflow != &workflow_hash {
                    return Err(WorkflowError::JournalDivergence(
                        "workflow_hash changed; journal cannot be resumed with a different script"
                            .into(),
                    ));
                }
                if bound_input != &input_hash {
                    return Err(WorkflowError::JournalDivergence(
                        "input_hash changed; journal cannot be resumed with different args".into(),
                    ));
                }
                Ok(())
            },
            _ => Err(WorkflowError::JournalDivergence(
                "journal identity is incomplete or inconsistent".into(),
            )),
        }
    }

    /// Returns the bound workflow content hash, if any.
    #[must_use]
    pub const fn workflow_hash(&self) -> Option<&ContentHash> {
        self.workflow_hash.as_ref()
    }

    /// Returns the bound input content hash, if any.
    #[must_use]
    pub const fn input_hash(&self) -> Option<&ContentHash> {
        self.input_hash.as_ref()
    }

    /// Returns the committed entries in invocation order.
    #[must_use]
    pub fn entries(&self) -> &[JournalEntry] {
        &self.entries
    }

    /// Rewinds the first failed invocation so it can be retried.
    ///
    /// Returns the number of agent slots reset for retry.
    pub fn retry_failed(&mut self) -> usize {
        let mut truncate_to = None;
        let mut reset = 0;

        for (index, entry) in self.entries.iter_mut().enumerate() {
            match entry {
                JournalEntry::Agent { result, .. } if !result.success || result.cancelled => {
                    reset = 1;
                    truncate_to = Some(index);
                    break;
                },
                JournalEntry::AgentAmbiguous { .. } => {
                    reset = 1;
                    truncate_to = Some(index);
                    break;
                },
                JournalEntry::Parallel { slots, .. } => {
                    for slot in slots {
                        let should_retry = match slot {
                            ParallelSlot::Pending => false,
                            ParallelSlot::Ambiguous { .. }
                            | ParallelSlot::Completed { result: None, .. } => true,
                            ParallelSlot::Completed {
                                result: Some(result),
                                ..
                            } => !result.success || result.cancelled,
                        };
                        if should_retry {
                            *slot = ParallelSlot::Pending;
                            reset += 1;
                        }
                    }
                    if reset > 0 {
                        truncate_to = Some(index + 1);
                        break;
                    }
                },
                _ => {},
            }
        }

        if let Some(length) = truncate_to {
            self.entries.truncate(length);
        }
        reset
    }

    /// Serializes the journal as deterministic JSON.
    ///
    /// # Errors
    ///
    /// Returns a JSON error if serialization fails.
    pub fn to_json(&self) -> Result<String, WorkflowError> {
        serde_json::to_string(self).map_err(WorkflowError::from)
    }

    /// Atomically writes this journal to a checkpoint path.
    ///
    /// The temporary file and its parent directory are `fsync`ed before the
    /// rename is acknowledged, so a successful return means the journal is
    /// durable (local RPO=0, matching celld's output gate).
    ///
    /// # Errors
    ///
    /// Returns an I/O or serialization error when the checkpoint cannot be written.
    pub fn write_atomic(
        &self,
        path: &Path,
    ) -> Result<(), WorkflowError> {
        use std::io::Write;

        let temporary = checkpoint_temporary_path(path);
        {
            let mut file = fs::File::create(&temporary)
                .map_err(|error| WorkflowError::io(&temporary, error))?;
            file.write_all(self.to_json()?.as_bytes())
                .map_err(|error| WorkflowError::io(&temporary, error))?;
            file.sync_all()
                .map_err(|error| WorkflowError::io(&temporary, error))?;
        }
        if let Err(error) = fs::rename(&temporary, path) {
            let _ = fs::remove_file(&temporary);
            return Err(WorkflowError::io(path, error));
        }
        sync_parent_directory(path)?;
        Ok(())
    }

    /// Deserializes and validates a journal.
    ///
    /// # Errors
    ///
    /// Returns [`WorkflowError::Json`] or [`WorkflowError::JournalDivergence`].
    pub fn from_json(json: &str) -> Result<Self, WorkflowError> {
        let raw: RawJournal = serde_json::from_str(json)?;
        Self::from_parts(raw.workflow_hash, raw.input_hash, raw.entries)
    }

    pub(crate) fn get(
        &self,
        invocation: usize,
    ) -> Option<&JournalEntry> {
        self.entries.get(invocation)
    }

    pub(crate) fn get_mut(
        &mut self,
        invocation: usize,
    ) -> Option<&mut JournalEntry> {
        self.entries.get_mut(invocation)
    }

    pub(crate) fn push(
        &mut self,
        entry: JournalEntry,
    ) {
        self.entries.push(entry);
    }

    pub(crate) fn pop(&mut self) -> Option<JournalEntry> {
        self.entries.pop()
    }
}

pub fn panel_content_hash(requests: &[AgentRequest]) -> Result<ContentHash, serde_json::Error> {
    crate::hash::hash_json(&serde_json::to_value(requests)?)
}

pub fn scratch_content_hash(
    name: &str,
    content: &str,
) -> Result<ContentHash, serde_json::Error> {
    crate::hash::hash_json(&serde_json::json!({
        "name": name,
        "content": content,
    }))
}

pub fn gate_content_hash(
    kind: &str,
    message: &str,
) -> Result<ContentHash, serde_json::Error> {
    crate::hash::hash_json(&serde_json::json!({
        "kind": kind,
        "message": message,
    }))
}

pub fn wake_content_hash(
    kind: &str,
    due_ms: i64,
) -> Result<ContentHash, serde_json::Error> {
    crate::hash::hash_json(&serde_json::json!({
        "kind": kind,
        "due_ms": due_ms,
    }))
}

pub fn result_content_hash(result: &AgentResult) -> Result<ContentHash, serde_json::Error> {
    crate::hash::hash_json(&serde_json::to_value(result)?)
}

pub fn optional_result_content_hash(
    result: Option<&AgentResult>
) -> Result<ContentHash, serde_json::Error> {
    crate::hash::hash_json(&serde_json::to_value(result)?)
}

fn validate_agent_hashes(
    expected: usize,
    request_hash: &ContentHash,
    result_hash: &ContentHash,
    request: &AgentRequest,
    result: &AgentResult,
) -> Result<(), WorkflowError> {
    let computed_request = request.content_hash()?;
    if &computed_request != request_hash {
        return Err(WorkflowError::JournalDivergence(format!(
            "agent entry {expected} request_hash does not match request payload"
        )));
    }
    let computed_result = result_content_hash(result)?;
    if &computed_result != result_hash {
        return Err(WorkflowError::JournalDivergence(format!(
            "agent entry {expected} result_hash does not match result payload"
        )));
    }
    Ok(())
}

fn validate_parallel_hashes(
    expected: usize,
    panel_hash: &ContentHash,
    requests: &[AgentRequest],
    slots: &[ParallelSlot],
) -> Result<(), WorkflowError> {
    let computed = panel_content_hash(requests)?;
    if &computed != panel_hash {
        return Err(WorkflowError::JournalDivergence(format!(
            "parallel entry {expected} panel_hash does not match requests"
        )));
    }
    for (index, slot) in slots.iter().enumerate() {
        if let ParallelSlot::Completed {
            result_hash,
            result,
        } = slot
        {
            let computed = optional_result_content_hash(result.as_ref())?;
            if &computed != result_hash {
                return Err(WorkflowError::JournalDivergence(format!(
                    "parallel entry {expected} slot {index} result_hash does not match result"
                )));
            }
        }
    }
    Ok(())
}

fn validate_scratch_hashes(
    expected: usize,
    kind: &str,
    content_hash: &ContentHash,
    name: &str,
    content: &str,
) -> Result<(), WorkflowError> {
    validate_scratch_name(name)?;
    let computed = scratch_content_hash(name, content)?;
    if &computed != content_hash {
        return Err(WorkflowError::JournalDivergence(format!(
            "{kind} entry {expected} content_hash does not match payload"
        )));
    }
    Ok(())
}

fn validate_scratch_name(name: &str) -> Result<(), WorkflowError> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\\')
        || name.contains('\0')
    {
        return Err(WorkflowError::JournalDivergence(format!(
            "invalid scratch file name `{name}`"
        )));
    }
    Ok(())
}

fn checkpoint_temporary_path(path: &Path) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut temporary = OsString::from(path.as_os_str());
    temporary.push(format!(
        ".tmp-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    PathBuf::from(temporary)
}

fn sync_parent_directory(path: &Path) -> Result<(), WorkflowError> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    let parent = if parent.as_os_str().is_empty() {
        Path::new(".")
    } else {
        parent
    };
    let dir = fs::File::open(parent).map_err(|error| WorkflowError::io(parent, error))?;
    dir.sync_all()
        .map_err(|error| WorkflowError::io(parent, error))?;
    Ok(())
}

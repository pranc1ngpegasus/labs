use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

use crate::{AgentRequest, AgentResult, WorkflowError};

/// The durable state of one slot in a parallel panel.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ParallelSlot {
    /// The panel was atomically admitted, but this slot has not committed yet.
    Pending,
    /// The slot committed; `None` represents an infrastructure-failed slot.
    Completed {
        /// The committed agent result, or `None` for a failed slot.
        result: Option<AgentResult>,
    },
}

/// One committed host call and its deterministic result.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "call", rename_all = "snake_case")]
pub enum JournalEntry {
    /// A single agent invocation.
    Agent {
        /// Zero-based host-call sequence number.
        invocation: usize,
        /// Original request, used to detect replay divergence.
        request: AgentRequest,
        /// Committed host result.
        result: AgentResult,
    },
    /// A barrier-style parallel panel.
    Parallel {
        /// Zero-based host-call sequence number.
        invocation: usize,
        /// Original ordered requests.
        requests: Vec<AgentRequest>,
        /// Ordered durable slot states, one per request.
        slots: Vec<ParallelSlot>,
    },
    /// A scratch-file write.
    WriteScratch {
        /// Zero-based host-call sequence number.
        invocation: usize,
        /// Per-run scratch name.
        name: String,
        /// Written content.
        content: String,
    },
    /// A scratch-file read.
    ReadScratch {
        /// Zero-based host-call sequence number.
        invocation: usize,
        /// Per-run scratch name.
        name: String,
        /// Content observed by the read.
        content: String,
    },
    /// A user gate that has already paused once.
    AwaitUser {
        /// Zero-based host-call sequence number.
        invocation: usize,
        /// Gate kind.
        kind: String,
        /// Gate message.
        message: String,
    },
}

impl JournalEntry {
    /// Returns the stable invocation number of this entry.
    #[must_use]
    pub const fn invocation(&self) -> usize {
        match self {
            Self::Agent { invocation, .. }
            | Self::Parallel { invocation, .. }
            | Self::WriteScratch { invocation, .. }
            | Self::ReadScratch { invocation, .. }
            | Self::AwaitUser { invocation, .. } => *invocation,
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
        if let Self::Parallel {
            requests, slots, ..
        } = self
            && requests.len() != slots.len()
        {
            return Err(WorkflowError::JournalDivergence(format!(
                "parallel entry {expected} has {} request(s) but {} slot state(s)",
                requests.len(),
                slots.len()
            )));
        }
        Ok(())
    }
}

/// Ordered committed host-call results for one workflow run.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Journal {
    entries: Vec<JournalEntry>,
}

/// The raw, unvalidated wire shape of a [`Journal`].
///
/// Kept separate from [`Journal`] so that deserialization can happen without the
/// validating [`Deserialize`] impl, letting [`Journal::from_json`] route failures
/// through [`Journal::from_entries`] and preserve the [`WorkflowError::JournalDivergence`]
/// error type.
#[derive(Deserialize)]
struct RawJournal {
    entries: Vec<JournalEntry>,
}

impl<'de> Deserialize<'de> for Journal {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = RawJournal::deserialize(deserializer)?;
        Self::from_entries(raw.entries).map_err(D::Error::custom)
    }
}

impl Journal {
    /// Creates an empty journal.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Creates a journal from pre-existing entries and validates all invariants.
    ///
    /// Pending parallel slots are valid durable state. Every parallel entry must
    /// still contain exactly one slot state per request.
    ///
    /// # Errors
    ///
    /// Returns [`WorkflowError::JournalDivergence`] when invocation indices are
    /// not contiguous and zero-based or an entry-specific invariant is invalid.
    pub fn from_entries(entries: Vec<JournalEntry>) -> Result<Self, WorkflowError> {
        let journal = Self { entries };
        journal.validate()?;
        Ok(journal)
    }

    /// Validates invocation indices and entry-specific invariants.
    ///
    /// # Errors
    ///
    /// Returns [`WorkflowError::JournalDivergence`] for a non-contiguous index
    /// or invalid entry such as a parallel panel with mismatched cardinality.
    pub fn validate(&self) -> Result<(), WorkflowError> {
        for (expected, entry) in self.entries.iter().enumerate() {
            entry.validate(expected)?;
        }
        Ok(())
    }

    /// Returns the committed entries in invocation order.
    #[must_use]
    pub fn entries(&self) -> &[JournalEntry] {
        &self.entries
    }

    /// Rewinds the first failed invocation so it can be retried.
    ///
    /// Successful entries before the failure are preserved. A failed sequential
    /// agent and every later entry are removed. For a parallel panel, only
    /// failed, cancelled, or infrastructure-failed slots are reset to pending;
    /// successful sibling slots remain committed, and later dependent entries
    /// are removed.
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
                JournalEntry::Parallel { slots, .. } => {
                    for slot in slots {
                        let should_retry = match slot {
                            ParallelSlot::Pending => false,
                            ParallelSlot::Completed { result: None } => true,
                            ParallelSlot::Completed {
                                result: Some(result),
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
    /// The complete JSON is first written to a sibling temporary file and then
    /// renamed over the checkpoint, so an interrupted write cannot leave a
    /// partially serialized journal at the requested path.
    ///
    /// # Errors
    ///
    /// Returns an I/O or serialization error when the checkpoint cannot be
    /// written.
    pub fn write_atomic(
        &self,
        path: &Path,
    ) -> Result<(), WorkflowError> {
        let temporary = checkpoint_temporary_path(path);
        fs::write(&temporary, self.to_json()?)
            .map_err(|error| WorkflowError::io(&temporary, error))?;
        if let Err(error) = fs::rename(&temporary, path) {
            let _ = fs::remove_file(&temporary);
            return Err(WorkflowError::io(path, error));
        }
        Ok(())
    }

    /// Deserializes and validates a journal.
    ///
    /// Deserialization uses the raw, unvalidated wire shape so that validation
    /// failures are reported as [`WorkflowError::JournalDivergence`] — matching
    /// [`Journal::from_entries`] and [`Engine::run`](crate::Engine::run) — rather
    /// than being wrapped as a JSON error.
    ///
    /// # Errors
    ///
    /// Returns [`WorkflowError::Json`] for malformed input, or
    /// [`WorkflowError::JournalDivergence`] for invalid invocation indices or
    /// entry-specific invariants such as parallel cardinality.
    pub fn from_json(json: &str) -> Result<Self, WorkflowError> {
        let raw: RawJournal = serde_json::from_str(json)?;
        Self::from_entries(raw.entries)
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
}

fn checkpoint_temporary_path(path: &Path) -> PathBuf {
    let mut temporary = OsString::from(path.as_os_str());
    temporary.push(format!(".tmp-{}", std::process::id()));
    PathBuf::from(temporary)
}

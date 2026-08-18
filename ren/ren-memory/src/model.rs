use std::{
    collections::BTreeMap,
    fmt,
    path::{Path, PathBuf},
    str::FromStr,
};

use serde::{Deserialize, Serialize};
use ulid::Ulid;
use yaml_serde::Value as YamlValue;

use crate::error::{MemoryError, Result};

pub const SCHEMA: &str = "ren-memory/v1";
pub const MAX_FRONTMATTER_BYTES: usize = 1024 * 1024;
pub const MAX_NOTE_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, clap::ValueEnum)]
#[value(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum NoteType {
    Fleeting,
    Literature,
    Permanent,
    Structure,
    Index,
}

impl NoteType {
    #[must_use]
    pub const fn directory(self) -> &'static str {
        match self {
            Self::Fleeting => "fleeting",
            Self::Literature => "literature",
            Self::Permanent => "permanent",
            Self::Structure => "structure",
            Self::Index => "index",
        }
    }
}

impl fmt::Display for NoteType {
    fn fmt(
        &self,
        formatter: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        formatter.write_str(self.directory())
    }
}

impl FromStr for NoteType {
    type Err = MemoryError;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "fleeting" => Ok(Self::Fleeting),
            "literature" => Ok(Self::Literature),
            "permanent" => Ok(Self::Permanent),
            "structure" => Ok(Self::Structure),
            "index" => Ok(Self::Index),
            _ => Err(MemoryError::Validation(format!(
                "unknown note type `{value}`"
            ))),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, clap::ValueEnum)]
#[value(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum NoteState {
    Inbox,
    Proposed,
    Accepted,
    NeedsContext,
    Rejected,
    Archived,
}

impl fmt::Display for NoteState {
    fn fmt(
        &self,
        formatter: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        let value = match self {
            Self::Inbox => "inbox",
            Self::Proposed => "proposed",
            Self::Accepted => "accepted",
            Self::NeedsContext => "needs_context",
            Self::Rejected => "rejected",
            Self::Archived => "archived",
        };
        formatter.write_str(value)
    }
}

impl FromStr for NoteState {
    type Err = MemoryError;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "inbox" => Ok(Self::Inbox),
            "proposed" => Ok(Self::Proposed),
            "accepted" => Ok(Self::Accepted),
            "needs_context" => Ok(Self::NeedsContext),
            "rejected" => Ok(Self::Rejected),
            "archived" => Ok(Self::Archived),
            _ => Err(MemoryError::Validation(format!(
                "unknown note state `{value}`"
            ))),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, clap::ValueEnum)]
#[value(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum Relation {
    Supports,
    Contradicts,
    Refines,
    ExampleOf,
    Related,
    Sequence,
    MemberOfStructure,
    SourceOf,
    PromotedTo,
    Supersedes,
}

impl fmt::Display for Relation {
    fn fmt(
        &self,
        formatter: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        let value = match self {
            Self::Supports => "supports",
            Self::Contradicts => "contradicts",
            Self::Refines => "refines",
            Self::ExampleOf => "example_of",
            Self::Related => "related",
            Self::Sequence => "sequence",
            Self::MemberOfStructure => "member_of_structure",
            Self::SourceOf => "source_of",
            Self::PromotedTo => "promoted_to",
            Self::Supersedes => "supersedes",
        };
        formatter.write_str(value)
    }
}

impl FromStr for Relation {
    type Err = MemoryError;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "supports" => Ok(Self::Supports),
            "contradicts" => Ok(Self::Contradicts),
            "refines" => Ok(Self::Refines),
            "example_of" => Ok(Self::ExampleOf),
            "related" => Ok(Self::Related),
            "sequence" => Ok(Self::Sequence),
            "member_of_structure" => Ok(Self::MemberOfStructure),
            "source_of" => Ok(Self::SourceOf),
            "promoted_to" => Ok(Self::PromotedTo),
            "supersedes" => Ok(Self::Supersedes),
            _ => Err(MemoryError::Validation(format!(
                "unknown relation `{value}`"
            ))),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Link {
    pub to: String,
    pub rel: Relation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Source {
    pub kind: String,
    #[serde(flatten)]
    pub fields: BTreeMap<String, YamlValue>,
}

/// A dependency on another managed note or an explicitly external resource.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(untagged)]
pub enum Dependency {
    Local(String),
    External { external: String },
}

impl Dependency {
    #[must_use]
    pub fn local_id(&self) -> Option<&str> {
        match self {
            Self::Local(id) => Some(id),
            Self::External { .. } => None,
        }
    }

    fn validate(&self) -> Result<()> {
        match self {
            Self::Local(id) => validate_id(id),
            Self::External { external } if external.trim().is_empty() => Err(
                MemoryError::Validation("external dependency must not be empty".into()),
            ),
            Self::External { .. } => Ok(()),
        }
    }

    fn sort_key(&self) -> (&str, &str) {
        match self {
            Self::Local(id) => ("local", id),
            Self::External { external } => ("external", external),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Frontmatter {
    pub schema: String,
    pub id: String,
    #[serde(rename = "type")]
    pub note_type: NoteType,
    pub state: NoteState,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deps: Vec<Dependency>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<Link>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<Source>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub promoted_from: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supersedes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, YamlValue>,
}

impl Frontmatter {
    /// Validates and normalizes managed frontmatter.
    ///
    /// # Errors
    ///
    /// Returns a validation error for unsupported schemas, malformed IDs, empty
    /// required values, or accepted links without reasons.
    pub fn validate(&mut self) -> Result<()> {
        if self.schema != SCHEMA {
            return Err(MemoryError::Validation(format!(
                "unsupported note schema `{}`",
                self.schema
            )));
        }
        validate_id(&self.id)?;
        if self.created_at.trim().is_empty() {
            return Err(MemoryError::Validation(
                "created_at must not be empty".into(),
            ));
        }
        validate_timestamp("created_at", &self.created_at)?;
        if let Some(updated_at) = &self.updated_at {
            validate_timestamp("updated_at", updated_at)?;
        }
        if self
            .title
            .as_ref()
            .is_some_and(|title| title.trim().is_empty())
        {
            return Err(MemoryError::Validation(
                "title must not be empty when present".into(),
            ));
        }
        for dependency in &self.deps {
            dependency.validate()?;
        }
        for link in &self.links {
            validate_id(&link.to)?;
            if link
                .reason
                .as_ref()
                .is_none_or(|reason| reason.trim().is_empty())
            {
                return Err(MemoryError::Validation(format!(
                    "link {} -> {} requires a reason",
                    link.rel, link.to
                )));
            }
        }
        for id in self.promoted_from.iter().chain(self.supersedes.iter()) {
            validate_id(id)?;
        }
        self.deps
            .sort_by(|left, right| left.sort_key().cmp(&right.sort_key()));
        self.deps.dedup();
        self.links.sort_by(|left, right| {
            (&left.to, left.rel.to_string()).cmp(&(&right.to, right.rel.to_string()))
        });
        self.links
            .dedup_by(|left, right| left.to == right.to && left.rel == right.rel);
        self.promoted_from.sort();
        self.promoted_from.dedup();
        self.supersedes.sort();
        self.supersedes.dedup();
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Note {
    pub frontmatter: Frontmatter,
    pub body: String,
}

impl Note {
    /// Parses a complete managed Markdown note.
    ///
    /// # Errors
    ///
    /// Returns an input, YAML, validation, or path error when the note is not a
    /// valid `ren-memory/v1` document.
    pub fn parse(
        path: &Path,
        input: &str,
    ) -> Result<Self> {
        if input.len() > MAX_NOTE_BYTES {
            return Err(MemoryError::InputTooLarge {
                limit: MAX_NOTE_BYTES,
            });
        }
        let normalized = input.replace("\r\n", "\n");
        let Some(rest) = normalized.strip_prefix("---\n") else {
            return Err(invalid_note(path, "missing opening YAML delimiter"));
        };
        let Some(boundary) = rest.find("\n---\n") else {
            return Err(invalid_note(path, "missing closing YAML delimiter"));
        };
        if boundary > MAX_FRONTMATTER_BYTES {
            return Err(MemoryError::InputTooLarge {
                limit: MAX_FRONTMATTER_BYTES,
            });
        }
        let yaml = &rest[..boundary];
        let mut frontmatter: Frontmatter = yaml_serde::from_str(yaml)
            .map_err(|error| invalid_note(path, &format!("invalid YAML: {error}")))?;
        frontmatter
            .validate()
            .map_err(|error| invalid_note(path, &error.to_string()))?;
        let body = rest[boundary + "\n---\n".len()..].to_owned();
        let note = Self { frontmatter, body };
        note.validate_path(path)?;
        Ok(note)
    }

    /// Confirms that the filename and directory agree with the note metadata.
    ///
    /// # Errors
    ///
    /// Returns an invalid-note error when the stable ID is not the filename or
    /// the type does not match the containing managed directory.
    pub fn validate_path(
        &self,
        path: &Path,
    ) -> Result<()> {
        let expected = format!("{}.md", self.frontmatter.id);
        if path.file_name().and_then(|value| value.to_str()) != Some(expected.as_str()) {
            return Err(invalid_note(
                path,
                &format!("filename must be `{expected}`"),
            ));
        }
        if path
            .parent()
            .and_then(Path::file_name)
            .and_then(|value| value.to_str())
            .is_some_and(|directory| {
                directory != self.frontmatter.note_type.directory() && directory != "archived"
            })
        {
            return Err(invalid_note(
                path,
                &format!(
                    "note type `{}` does not match its directory",
                    self.frontmatter.note_type
                ),
            ));
        }
        Ok(())
    }

    /// Serializes the note as portable Markdown with YAML frontmatter.
    ///
    /// # Errors
    ///
    /// Returns a YAML serialization error when metadata cannot be encoded.
    pub fn to_markdown(&self) -> Result<String> {
        let yaml = yaml_serde::to_string(&self.frontmatter)?;
        let markdown = format!("---\n{}---\n{}", yaml, self.body);
        if markdown.len() > MAX_NOTE_BYTES {
            return Err(MemoryError::InputTooLarge {
                limit: MAX_NOTE_BYTES,
            });
        }
        Ok(markdown)
    }

    #[must_use]
    pub fn new(
        note_type: NoteType,
        state: NoteState,
        created_at: String,
        title: Option<String>,
        body: String,
    ) -> Self {
        Self {
            frontmatter: Frontmatter {
                schema: SCHEMA.into(),
                id: Ulid::generate().to_string(),
                note_type,
                state,
                created_at,
                title,
                updated_at: None,
                project: None,
                tags: Vec::new(),
                deps: Vec::new(),
                links: Vec::new(),
                sources: Vec::new(),
                promoted_from: Vec::new(),
                supersedes: Vec::new(),
                aliases: Vec::new(),
                extra: BTreeMap::new(),
            },
            body,
        }
    }
}

pub fn read_note(path: &Path) -> Result<Note> {
    let input = std::fs::read_to_string(path).map_err(|error| MemoryError::io(path, error))?;
    Note::parse(path, &input)
}

pub fn validate_id(id: &str) -> Result<()> {
    if id.len() != 26 || id.to_ascii_uppercase() != id || id.parse::<Ulid>().is_err() {
        return Err(MemoryError::Validation(format!(
            "`{id}` is not a canonical ULID"
        )));
    }
    Ok(())
}

fn validate_timestamp(
    field: &str,
    value: &str,
) -> Result<()> {
    value.parse::<jiff::Timestamp>().map_err(|error| {
        MemoryError::Validation(format!("{field} must be an RFC3339 timestamp: {error}"))
    })?;
    Ok(())
}

fn invalid_note(
    path: &Path,
    message: &str,
) -> MemoryError {
    MemoryError::InvalidNote {
        path: PathBuf::from(path),
        message: message.into(),
    }
}

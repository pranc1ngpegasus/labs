//! Safe application of one-file Git unified diffs.
//!
//! The edit protocol is deliberately line-oriented: the caller supplies a
//! complete patch, `gitpatch` validates its hunk headers, and Git applies the
//! patch after a durable write-ahead check.  No search-and-replace or direct
//! file rewrite is performed here.

use std::{
    fmt::Write as _,
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use gitpatch::Patch;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{Tool, ToolFuture, ToolsError};

static PATCH_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Validates a single-file unified diff.
///
/// # Errors
///
/// Returns [`ToolsError::Edit`] when the diff is empty or malformed. Hunk
/// counts are recalculated from the hunk body; counts supplied by the caller
/// are not trusted.
pub fn validate_unified_diff(response: &str) -> Result<(), ToolsError> {
    if response.trim().is_empty() {
        return Err(ToolsError::Edit("patch is empty".into()));
    }
    let normalized = normalize_unified_diff(response)?;
    validate_with_parser(&normalized)?;
    validate_hunk_counts(&normalized)
}

fn validate_with_parser(response: &str) -> Result<(), ToolsError> {
    if Patch::from_single(response).is_err() {
        // Keep accepting valid Git dialects that gitpatch rejects; the local
        // structural validator and `git apply --check` are authoritative.
        validate_hunk_counts(response)?;
    }
    Ok(())
}

fn parse_hunk_range(value: &str) -> Result<(usize, usize), ToolsError> {
    let value = value
        .strip_prefix(['-', '+'])
        .ok_or_else(|| ToolsError::Edit(format!("invalid hunk range `{value}`")))?;
    let (start, count) = value.split_once(',').map_or((value, "1"), |parts| parts);
    let start = start
        .parse()
        .map_err(|_| ToolsError::Edit(format!("invalid hunk start `{start}`")))?;
    let count = count
        .parse()
        .map_err(|_| ToolsError::Edit(format!("invalid hunk count `{count}`")))?;
    Ok((start, count))
}

fn hunk_line_counts(lines: &[&str]) -> Result<(usize, usize), ToolsError> {
    let mut old = 0;
    let mut new = 0;
    for line in lines {
        match line.as_bytes().first().copied() {
            Some(b' ') => {
                old += 1;
                new += 1;
            },
            Some(b'-') => old += 1,
            Some(b'+') => new += 1,
            Some(b'\\') => {},
            _ => {
                return Err(ToolsError::Edit(format!("invalid line in hunk `{line}`")));
            },
        }
    }
    Ok((old, new))
}

fn normalize_hunk_headers(response: &str) -> Result<String, ToolsError> {
    let lines: Vec<&str> = response.split_inclusive('\n').collect();
    let mut normalized = String::with_capacity(response.len());
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index];
        if !line.starts_with("@@ ") {
            normalized.push_str(line);
            index += 1;
            continue;
        }
        let header = line.trim_end_matches(['\r', '\n']);
        let (body, suffix) = header
            .strip_prefix("@@ ")
            .and_then(|body| body.split_once(" @@"))
            .ok_or_else(|| ToolsError::Edit(format!("invalid hunk header `{header}`")))?;
        let mut ranges = body.split_whitespace();
        let (old_start, _) = parse_hunk_range(
            ranges
                .next()
                .ok_or_else(|| ToolsError::Edit("hunk is missing old range".into()))?,
        )?;
        let (new_start, _) = parse_hunk_range(
            ranges
                .next()
                .ok_or_else(|| ToolsError::Edit("hunk is missing new range".into()))?,
        )?;
        if ranges.next().is_some() {
            return Err(ToolsError::Edit(format!("invalid hunk header `{header}`")));
        }
        let content_start = index + 1;
        let mut content_end = content_start;
        while content_end < lines.len()
            && !lines[content_end].starts_with("@@ ")
            && !lines[content_end].starts_with("diff --git ")
        {
            content_end += 1;
        }
        let (old_count, new_count) = hunk_line_counts(&lines[content_start..content_end])?;
        let ending = line.strip_prefix(header).unwrap_or("");
        let _ = write!(
            normalized,
            "@@ -{old_start},{old_count} +{new_start},{new_count} @@{suffix}{ending}"
        );
        index = content_start;
    }
    Ok(normalized)
}

fn normalize_unified_diff(response: &str) -> Result<String, ToolsError> {
    let mut normalized = normalize_hunk_headers(response)?;
    if !normalized.ends_with('\n') {
        normalized.push('\n');
    }
    Ok(normalized)
}

fn validate_hunk_counts(response: &str) -> Result<(), ToolsError> {
    let mut lines = response.lines().peekable();
    let mut hunks = 0;
    while let Some(line) = lines.next() {
        if !line.starts_with("@@ ") {
            continue;
        }
        let body = line
            .strip_prefix("@@ ")
            .and_then(|body| body.split_once(" @@").map(|(body, _)| body))
            .ok_or_else(|| ToolsError::Edit(format!("invalid hunk header `{line}`")))?;
        let mut ranges = body.split_whitespace();
        let (_, old_count) = parse_hunk_range(
            ranges
                .next()
                .ok_or_else(|| ToolsError::Edit("hunk is missing old range".into()))?,
        )?;
        let (_, new_count) = parse_hunk_range(
            ranges
                .next()
                .ok_or_else(|| ToolsError::Edit("hunk is missing new range".into()))?,
        )?;
        if ranges.next().is_some() {
            return Err(ToolsError::Edit(format!("invalid hunk header `{line}`")));
        }
        let mut seen_old = 0;
        let mut seen_new = 0;
        while let Some(next) = lines.peek().copied() {
            if next.starts_with("@@ ") || next.starts_with("diff --git ") {
                break;
            }
            let next = lines.next().unwrap_or_default();
            match next.as_bytes().first().copied() {
                Some(b' ') => {
                    seen_old += 1;
                    seen_new += 1;
                },
                Some(b'-') => seen_old += 1,
                Some(b'+') => seen_new += 1,
                Some(b'\\') => {},
                _ => {
                    return Err(ToolsError::Edit(format!("invalid line in hunk `{next}`")));
                },
            }
        }
        if seen_old != old_count || seen_new != new_count {
            return Err(ToolsError::Edit(format!(
                "hunk header counts {old_count}/{new_count}, found {seen_old}/{seen_new}"
            )));
        }
        hunks += 1;
    }
    if hunks == 0 {
        return Err(ToolsError::Edit("patch contains no hunks".into()));
    }
    Ok(())
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

async fn run_git(
    cwd: &Path,
    args: &[&str],
) -> Result<crate::CommandOutput, ToolsError> {
    let mut command = String::from("git");
    for arg in args {
        command.push(' ');
        command.push_str(&shell_quote(arg));
    }
    crate::run_line(&command, Some(cwd), Duration::from_secs(30)).await
}

fn git_error(
    operation: &str,
    output: &crate::CommandOutput,
) -> ToolsError {
    ToolsError::Edit(format!(
        "{operation} failed (exit {:?}): {}",
        output.code,
        output.stderr.trim()
    ))
}

async fn repository_root(file: &Path) -> Result<PathBuf, ToolsError> {
    let absolute = absolute_target_path(file)?;
    let mut probe = absolute
        .parent()
        .ok_or_else(|| ToolsError::Edit("target has no parent directory".into()))?;
    while !probe.exists() {
        probe = probe
            .parent()
            .ok_or_else(|| ToolsError::Edit("target has no existing parent directory".into()))?;
    }
    let probe = std::fs::canonicalize(probe).map_err(|source| ToolsError::io(probe, source))?;
    let output = run_git(&probe, &["rev-parse", "--show-toplevel"]).await?;
    if output.code != Some(0) {
        return Err(git_error("git rev-parse", &output));
    }
    let root = PathBuf::from(output.stdout.trim());
    std::fs::canonicalize(&root).map_err(|source| ToolsError::io(&root, source))
}

fn absolute_target_path(file: &Path) -> Result<PathBuf, ToolsError> {
    if file.is_absolute() {
        Ok(file.to_path_buf())
    } else {
        Ok(std::env::current_dir()
            .map_err(|source| ToolsError::io("current directory", source))?
            .join(file))
    }
}

fn relative_target(
    root: &Path,
    file: &Path,
) -> Result<PathBuf, ToolsError> {
    let absolute = absolute_target_path(file)?;
    let absolute = if absolute.exists() {
        std::fs::canonicalize(&absolute).map_err(|source| ToolsError::io(&absolute, source))?
    } else {
        let mut missing = Vec::new();
        let mut probe = absolute.as_path();
        while !probe.exists() {
            missing.push(
                probe
                    .file_name()
                    .ok_or_else(|| ToolsError::Edit("target has no file name".into()))?,
            );
            probe = probe.parent().ok_or_else(|| {
                ToolsError::Edit("target has no existing parent directory".into())
            })?;
        }
        let mut resolved =
            std::fs::canonicalize(probe).map_err(|source| ToolsError::io(probe, source))?;
        for component in missing.into_iter().rev() {
            resolved.push(component);
        }
        resolved
    };
    let relative = absolute
        .strip_prefix(root)
        .map_err(|_| ToolsError::Edit("target must be inside the Git worktree".into()))?;
    if relative.as_os_str().is_empty()
        || relative.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir | std::path::Component::RootDir
            )
        })
    {
        return Err(ToolsError::Edit(
            "target must be a file below the worktree".into(),
        ));
    }
    Ok(relative.to_path_buf())
}

fn header_path(path: &str) -> Option<&str> {
    match path {
        "/dev/null" => None,
        path if path.starts_with("a/") || path.starts_with("b/") => Some(&path[2..]),
        path => Some(path),
    }
}

fn patch_paths(response: &str) -> Result<(&str, &str), ToolsError> {
    let mut old = None;
    let mut new = None;
    for line in response.lines() {
        if let Some(path) = line.strip_prefix("--- ") {
            old = Some(path.split_once('\t').map_or(path, |(path, _)| path));
        } else if let Some(path) = line.strip_prefix("+++ ") {
            new = Some(path.split_once('\t').map_or(path, |(path, _)| path));
        }
    }
    match (old, new) {
        (Some(old), Some(new)) => Ok((old, new)),
        _ => Err(ToolsError::Edit(
            "patch must contain --- and +++ file headers".into(),
        )),
    }
}

fn check_target(
    root: &Path,
    file: &Path,
    response: &str,
) -> Result<(), ToolsError> {
    let target = relative_target(root, file)?;
    let (old, new) = patch_paths(response)?;
    let old = header_path(old.trim_matches('"'));
    let new = header_path(new.trim_matches('"'));
    if old != Some(target.to_string_lossy().as_ref())
        && new != Some(target.to_string_lossy().as_ref())
    {
        return Err(ToolsError::Edit(format!(
            "patch does not target requested file `{}`",
            target.display()
        )));
    }
    if old.is_some() && new.is_some() && old != new {
        return Err(ToolsError::Edit("renames are not supported by edit".into()));
    }
    Ok(())
}

fn write_ahead_patch(response: &str) -> Result<PathBuf, ToolsError> {
    let sequence = PATCH_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let path =
        std::env::temp_dir().join(format!("sui-edit-{}-{sequence}.patch", std::process::id()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options
        .open(&path)
        .map_err(|source| ToolsError::io(&path, source))?;
    let write_result = file
        .write_all(response.as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|source| ToolsError::io(&path, source));
    if let Err(error) = write_result {
        let _ = std::fs::remove_file(&path);
        return Err(error);
    }
    Ok(path)
}

/// Shared `git apply` flags (used by both `--check` and the real apply).
///
/// Adopted for LLM-generated diffs:
/// - `--ignore-whitespace`: locate hunks despite indent / tab-vs-space drift
/// - `--unidiff-zero`: accept hunks with no context lines (`git diff -U0`)
///
/// Rejected:
/// - `--3way`: needs `index` blob lines, implies `--index`, can write conflict
///   markers into the worktree
/// - `--recount`: hunk counts are already rewritten by [`normalize_hunk_headers`]
/// - `-C1`: does not absorb wrong context; further reduction (`-C0`) applies at
///   the wrong site
const GIT_APPLY_FLAGS: &[&str] = &[
    "--whitespace=nowarn",
    "--ignore-whitespace",
    "--unidiff-zero",
];

fn git_apply_args(
    check: bool,
    patch: &str,
) -> Vec<&str> {
    let mut args = vec!["apply"];
    if check {
        args.push("--check");
    }
    args.extend_from_slice(GIT_APPLY_FLAGS);
    args.push(patch);
    args
}

async fn apply_patch(
    root: &Path,
    patch: &Path,
) -> Result<(), ToolsError> {
    let patch = patch.to_string_lossy();
    let check = run_git(root, &git_apply_args(true, patch.as_ref())).await?;
    if check.code != Some(0) {
        return Err(git_error("git apply --check", &check));
    }
    let applied = run_git(root, &git_apply_args(false, patch.as_ref())).await?;
    if applied.code != Some(0) {
        return Err(git_error("git apply", &applied));
    }
    Ok(())
}

/// Applies one validated unified diff to the requested file through Git.
#[derive(Default)]
pub struct EditTool;

impl EditTool {
    /// Creates an [`EditTool`].
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Tool for EditTool {
    #[allow(clippy::unnecessary_literal_bound)]
    fn name(&self) -> &str {
        "edit"
    }

    #[allow(clippy::unnecessary_literal_bound)]
    fn description(&self) -> &str {
        "Apply one Git unified diff to the requested file. Hunk counts are recalculated from the hunk body, and the path must match `file`. For a new file use --- /dev/null and +++ b/path; for deletion use --- a/path and +++ /dev/null. Renames and multi-file patches are rejected. The patch is written ahead, checked, and applied by git."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "file": { "type": "string", "description": "Requested target path; must match the patch header" },
                "response": { "type": "string", "description": "One Git unified diff; hunk line counts are recalculated by the agent. New files: --- /dev/null and +++ b/path. Deletions: --- a/path and +++ /dev/null." }
            },
            "required": ["file", "response"],
            "additionalProperties": false
        })
    }

    fn call(
        &self,
        args: Value,
    ) -> ToolFuture<'_> {
        Box::pin(async move {
            let args: EditArgs = serde_json::from_value(args)
                .map_err(|error| ToolsError::InvalidArgs(error.to_string()))?;
            let response = normalize_unified_diff(&args.response)?;
            validate_with_parser(&response)?;
            validate_hunk_counts(&response)?;
            let root = repository_root(&args.file).await?;
            check_target(&root, &args.file, &response)?;
            let patch = write_ahead_patch(&response)?;
            let result = apply_patch(&root, &patch).await;
            let _ = std::fs::remove_file(&patch);
            result?;
            Ok(json!({ "file": args.file, "changed": true, "applied": true }))
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EditArgs {
    file: PathBuf,
    response: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ToolRegistry;
    use crate::corpus::{TempDir, temp_dir};
    use serde_json::json;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    const PATCH: &str = "diff --git a/sample.rs b/sample.rs\n--- a/sample.rs\n+++ b/sample.rs\n@@ -1,2 +1,2 @@\n-fn old() {}\n+fn new() {}\n fn main() {}\n";

    #[test]
    fn validates_hunk_headers() -> Result<(), ToolsError> {
        validate_unified_diff(PATCH)
    }

    #[test]
    fn recalculates_invalid_hunk_counts() -> Result<(), ToolsError> {
        let normalized = normalize_unified_diff("--- a/x\n+++ b/x\n@@ -1,2 +1,2 @@\n-old\n+new\n")?;
        assert!(normalized.contains("@@ -1,1 +1,1 @@"));
        validate_unified_diff(&normalized)
    }

    #[test]
    fn accepts_empty_added_line_without_trailing_newline() -> Result<(), ToolsError> {
        validate_unified_diff("--- a/x\n+++ b/x\n@@ -1 +1,2 @@\n-old\n+new\n+")
    }

    #[test]
    fn recognizes_dev_null_create_and_delete_paths() {
        assert_eq!(header_path("/dev/null"), None);
        assert_eq!(header_path("b/new.rs"), Some("new.rs"));
        assert_eq!(header_path("a/old.rs"), Some("old.rs"));
    }

    #[test]
    fn accepts_quoted_paths() -> Result<(), ToolsError> {
        validate_unified_diff(
            "--- \"a/old name.rs\"\n+++ \"b/old name.rs\"\n@@ -1 +1 @@\n-old\n+new\n",
        )
    }

    #[tokio::test]
    async fn applies_create_and_delete_through_git() -> Result<(), ToolsError> {
        let dir = TempDir(temp_dir("edit-git"));
        fs::create_dir_all(&dir.0).map_err(|source| ToolsError::io(&dir.0, source))?;
        init_git_repo(&dir.0)?;

        let mut registry = ToolRegistry::new();
        registry.register(EditTool);
        let file = dir.0.join("nested").join("new.rs");
        registry
            .call(
                "edit",
                json!({
                    "file": file,
                    "response": "diff --git a/nested/new.rs b/nested/new.rs\nnew file mode 100644\n--- /dev/null\n+++ b/nested/new.rs\n@@ -0,0 +1 @@\n+fn new() {}\n"
                }),
            )
            .await?;
        assert_eq!(
            fs::read_to_string(&file).map_err(|source| ToolsError::io(&file, source))?,
            "fn new() {}\n"
        );

        registry
            .call(
                "edit",
                json!({
                    "file": file,
                    "response": "diff --git a/nested/new.rs b/nested/new.rs\ndeleted file mode 100644\n--- a/nested/new.rs\n+++ /dev/null\n@@ -1 +0,0 @@\n-fn new() {}\n"
                }),
            )
            .await?;
        assert!(!file.exists());
        Ok(())
    }

    fn init_git_repo(dir: &Path) -> Result<(), ToolsError> {
        let init = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["init", "-q"])
            .status()
            .map_err(|source| ToolsError::io("git", source))?;
        assert!(init.success());
        Ok(())
    }

    async fn edit_file(
        dir: &Path,
        relative: &str,
        contents: &str,
        response: &str,
    ) -> Result<PathBuf, ToolsError> {
        fs::write(dir.join(relative), contents)
            .map_err(|source| ToolsError::io(dir.join(relative), source))?;
        let mut registry = ToolRegistry::new();
        registry.register(EditTool);
        let file = dir.join(relative);
        registry
            .call(
                "edit",
                json!({
                    "file": file,
                    "response": response
                }),
            )
            .await?;
        Ok(file)
    }

    #[tokio::test]
    async fn applies_despite_indent_drift() -> Result<(), ToolsError> {
        let dir = TempDir(temp_dir("edit-indent"));
        fs::create_dir_all(&dir.0).map_err(|source| ToolsError::io(&dir.0, source))?;
        init_git_repo(&dir.0)?;
        let file = edit_file(
            &dir.0,
            "sample.rs",
            "fn main() {\n    let x = 1;\n}\n",
            "diff --git a/sample.rs b/sample.rs\n--- a/sample.rs\n+++ b/sample.rs\n@@ -1,3 +1,3 @@\n fn main() {\n-  let x = 1;\n+    let x = 2;\n }\n",
        )
        .await?;
        assert_eq!(
            fs::read_to_string(&file).map_err(|source| ToolsError::io(&file, source))?,
            "fn main() {\n    let x = 2;\n}\n"
        );
        Ok(())
    }

    #[tokio::test]
    async fn applies_zero_context_hunk() -> Result<(), ToolsError> {
        let dir = TempDir(temp_dir("edit-u0"));
        fs::create_dir_all(&dir.0).map_err(|source| ToolsError::io(&dir.0, source))?;
        init_git_repo(&dir.0)?;
        let file = edit_file(
            &dir.0,
            "sample.rs",
            "fn old() {}\nfn main() {}\n",
            "diff --git a/sample.rs b/sample.rs\n--- a/sample.rs\n+++ b/sample.rs\n@@ -1,1 +1,1 @@\n-fn old() {}\n+fn new() {}\n",
        )
        .await?;
        assert_eq!(
            fs::read_to_string(&file).map_err(|source| ToolsError::io(&file, source))?,
            "fn new() {}\nfn main() {}\n"
        );
        Ok(())
    }

    #[tokio::test]
    async fn rejects_wrong_non_whitespace_context() -> Result<(), ToolsError> {
        let dir = TempDir(temp_dir("edit-wrong-ctx"));
        fs::create_dir_all(&dir.0).map_err(|source| ToolsError::io(&dir.0, source))?;
        init_git_repo(&dir.0)?;
        fs::write(dir.0.join("sample.rs"), "fn main() {\n    let x = 1;\n}\n")
            .map_err(|source| ToolsError::io(dir.0.join("sample.rs"), source))?;
        let mut registry = ToolRegistry::new();
        registry.register(EditTool);
        let error = registry
            .call(
                "edit",
                json!({
                    "file": dir.0.join("sample.rs"),
                    "response": "diff --git a/sample.rs b/sample.rs\n--- a/sample.rs\n+++ b/sample.rs\n@@ -1,3 +1,3 @@\n fn missing() {\n-    let x = 1;\n+    let x = 2;\n }\n"
                }),
            )
            .await
            .expect_err("wrong context must not apply");
        assert!(
            error.to_string().contains("git apply"),
            "unexpected error: {error}"
        );
        assert_eq!(
            fs::read_to_string(dir.0.join("sample.rs"))
                .map_err(|source| ToolsError::io(dir.0.join("sample.rs"), source))?,
            "fn main() {\n    let x = 1;\n}\n"
        );
        Ok(())
    }
}

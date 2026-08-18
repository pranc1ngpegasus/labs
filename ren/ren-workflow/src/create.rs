use std::{
    fs::{self, OpenOptions},
    io::{ErrorKind, Write as _},
    path::{Path, PathBuf},
};

use crate::{EchoHost, Engine, WorkflowError, bundled, meta, registry, store};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CreateTarget {
    Project,
    User,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreatePlan {
    pub path: PathBuf,
    pub contents: String,
}

pub fn store_dir(
    start: &Path,
    home: Option<&Path>,
    target: CreateTarget,
) -> Result<PathBuf, WorkflowError> {
    match target {
        CreateTarget::Project => Ok(registry::project_workflow_dir_in(start)),
        CreateTarget::User => home
            .map(registry::user_workflow_dir_in)
            .ok_or(WorkflowError::HomeUnavailable),
    }
}

pub fn create_plan(
    base_dir: &Path,
    name: &str,
    from_bundled: Option<&str>,
) -> Result<CreatePlan, WorkflowError> {
    let path = store::workflow_path(base_dir, name)?;
    let contents = if let Some(bundled_name) = from_bundled {
        copy_bundled(bundled_name, name)?
    } else {
        let contents = scaffold(name);
        validate_scaffold(&contents, name)?;
        contents
    };
    Ok(CreatePlan { path, contents })
}

pub fn write_plan(
    plan: &CreatePlan,
    force: bool,
) -> Result<(), WorkflowError> {
    if let Some(parent) = plan.path.parent() {
        fs::create_dir_all(parent).map_err(|error| WorkflowError::io(parent, error))?;
    }
    if force {
        fs::write(&plan.path, &plan.contents)
            .map_err(|error| WorkflowError::io(&plan.path, error))?;
        return Ok(());
    }

    let mut file = match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&plan.path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {
            return Err(WorkflowError::WorkflowExists(plan.path.clone()));
        },
        Err(error) => return Err(WorkflowError::io(&plan.path, error)),
    };
    file.write_all(plan.contents.as_bytes())
        .map_err(|error| WorkflowError::io(&plan.path, error))?;
    Ok(())
}

pub fn create_in(
    base_dir: &Path,
    name: &str,
    from_bundled: Option<&str>,
    force: bool,
) -> Result<CreatePlan, WorkflowError> {
    let plan = create_plan(base_dir, name, from_bundled)?;
    write_plan(&plan, force)?;
    Ok(plan)
}

fn scaffold(name: &str) -> String {
    format!(
        r#"let meta = #{{
    name: "{name}",
    description: "Describe what this workflow accomplishes",
    when_to_use: "Describe when an agent should select this workflow",
    phases: [
        #{{ title: "Work", detail: "Perform the workflow's main task" }}
    ],
    args_schema: #{{
        type: "object",
        properties: #{{}}
    }}
}};

// Announce progress with a phase title declared in meta.phases.
phase("Work");
// Replace this prompt and options with the work your workflow should delegate.
let result = agent("Perform the workflow task", #{{ label: "worker", capability_mode: "read-only" }});
// Return a JSON-compatible value as the workflow result.
complete(#{{ output: result.output }});
"#
    )
}

fn copy_bundled(
    bundled_name: &str,
    new_name: &str,
) -> Result<String, WorkflowError> {
    let workflow = bundled::find(bundled_name)
        .ok_or_else(|| WorkflowError::BundledWorkflowNotFound(bundled_name.to_owned()))?;
    rewrite_bundled_name(workflow.source, workflow.name, new_name)
}

pub fn rewrite_bundled_name(
    source: &str,
    bundled_name: &str,
    new_name: &str,
) -> Result<String, WorkflowError> {
    meta::validate_name(new_name)?;
    let metadata = meta::extract(source)?;
    if metadata.name != bundled_name {
        return Err(WorkflowError::BundledNameRewrite {
            bundled: bundled_name.to_owned(),
        });
    }

    let name_span =
        meta::name_value_span(source).map_err(|_| WorkflowError::BundledNameRewrite {
            bundled: bundled_name.to_owned(),
        })?;
    let prefix =
        source
            .get(..name_span.start)
            .ok_or_else(|| WorkflowError::BundledNameRewrite {
                bundled: bundled_name.to_owned(),
            })?;
    let suffix = source
        .get(name_span.end..)
        .ok_or_else(|| WorkflowError::BundledNameRewrite {
            bundled: bundled_name.to_owned(),
        })?;
    let mut rewritten = String::with_capacity(source.len() + new_name.len());
    rewritten.push_str(prefix);
    rewritten.push('"');
    rewritten.push_str(new_name);
    rewritten.push('"');
    rewritten.push_str(suffix);
    validate_bundled_copy(&rewritten, bundled_name, new_name)?;
    Ok(rewritten)
}

fn validate_scaffold(
    contents: &str,
    expected_name: &str,
) -> Result<(), WorkflowError> {
    let metadata = meta::extract(contents)?;
    if metadata.name != expected_name {
        return Err(WorkflowError::InvalidMeta(format!(
            "generated scaffold meta.name `{}` does not match requested name `{expected_name}`",
            metadata.name
        )));
    }
    Engine::new(EchoHost).compile(contents)?;
    Ok(())
}

fn validate_bundled_copy(
    contents: &str,
    bundled_name: &str,
    expected_name: &str,
) -> Result<(), WorkflowError> {
    let metadata = meta::extract(contents)?;
    if metadata.name != expected_name {
        return Err(WorkflowError::BundledNameRewrite {
            bundled: bundled_name.to_owned(),
        });
    }
    Engine::new(EchoHost).compile(contents)?;
    Ok(())
}

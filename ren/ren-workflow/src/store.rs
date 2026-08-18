use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::{WorkflowError, meta};

pub fn workflow_path(
    store_dir: &Path,
    name: &str,
) -> Result<PathBuf, WorkflowError> {
    meta::validate_name(name)?;
    Ok(store_dir.join(format!("{name}.rhai")))
}

pub fn remove_from_store(
    name: &str,
    store_dir: &Path,
) -> Result<PathBuf, WorkflowError> {
    let target = workflow_path(store_dir, name)?;
    if !target.is_file() {
        return Err(WorkflowError::WorkflowNotFound {
            name: name.to_owned(),
            path: target,
        });
    }
    fs::remove_file(&target).map_err(|error| WorkflowError::io(&target, error))?;
    Ok(target)
}

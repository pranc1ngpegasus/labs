//! CLI subcommands.

mod list;
mod record;

pub use list::ListArgs;
pub use record::RecordArgs;

use crate::MainError;

/// A subcommand, run to completion.
pub trait Run {
    /// # Errors
    ///
    /// Returns a [`MainError`] when the command fails.
    fn run(self) -> Result<(), MainError>;
}

//! CLI subcommands.

mod list;

pub use list::ListArgs;

use crate::MainError;

/// A subcommand, run to completion.
pub trait Run {
    /// # Errors
    ///
    /// Returns a [`MainError`] when the command fails.
    fn run(self) -> Result<(), MainError>;
}

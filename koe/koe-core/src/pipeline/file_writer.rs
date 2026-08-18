//! Async file writer for encoded audio and transcript output.

use std::path::Path;

use tokio::fs::File;
use tokio::io::AsyncWriteExt;

/// Buffered async writer for pipeline output files.
pub struct FileWriter {
    file: File,
    bytes_written: u64,
}

impl FileWriter {
    /// Creates (or truncates) the output file at `path`.
    ///
    /// # Errors
    ///
    /// Returns [`std::io::Error`] when the file cannot be created.
    pub async fn create(path: &Path) -> Result<Self, std::io::Error> {
        let file = File::create(path).await?;
        Ok(Self {
            file,
            bytes_written: 0,
        })
    }

    /// Appends encoded bytes to the file.
    ///
    /// # Errors
    ///
    /// Returns [`std::io::Error`] when the write fails.
    pub async fn write(
        &mut self,
        data: &[u8],
    ) -> Result<(), std::io::Error> {
        self.file.write_all(data).await?;
        self.bytes_written = self
            .bytes_written
            .saturating_add(u64::try_from(data.len()).unwrap_or(u64::MAX));
        Ok(())
    }

    /// Flushes buffered data to disk.
    ///
    /// # Errors
    ///
    /// Returns [`std::io::Error`] when flushing fails.
    pub async fn flush(&mut self) -> Result<(), std::io::Error> {
        self.file.flush().await
    }

    /// Total bytes written so far.
    #[must_use]
    pub const fn bytes_written(&self) -> u64 {
        self.bytes_written
    }
}

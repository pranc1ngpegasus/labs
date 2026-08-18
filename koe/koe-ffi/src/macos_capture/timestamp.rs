//! Monotonic timestamps for capture callbacks.

use std::sync::OnceLock;
use std::time::Instant;

/// Milliseconds since an arbitrary process-local origin (monotonic).
#[must_use]
pub fn monotonic_ms() -> u64 {
    static ORIGIN: OnceLock<Instant> = OnceLock::new();
    let origin = ORIGIN.get_or_init(Instant::now);
    u64::try_from(origin.elapsed().as_millis()).unwrap_or(u64::MAX)
}

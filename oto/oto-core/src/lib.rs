//! Oto core — recording pipeline and session control.
//!
//! Wires [`oto_capture`] and [`oto_encode`] together: a bounded channel with
//! drop-oldest backpressure, a consumer thread, and a recording session that
//! owns the capture-to-file lifecycle and statistics. Implemented in later
//! milestones per design 02.
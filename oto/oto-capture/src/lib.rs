//! Oto capture — device enumeration and microphone capture.
//!
//! Wraps [`shiguredo_audio_device`] so that platform-specific backend code and
//! feature selection stay confined to this crate. Device listing, device
//! selection, and capture sessions land here in later milestones.

//! Opus comment tags built from a recording session (RFC 7845 `OpusTags`).
//!
//! The Ogg/Opus encoder itself lives in `oto-encode`; this module owns the
//! koe-specific comment set (`TITLE`/`ARTIST`/`DATE`/`DESCRIPTION`/`ENCODER`/
//! `KOE_SOURCE`) derived from a capture source and locale.

use std::fmt::Write as _;
use std::time::{SystemTime, UNIX_EPOCH};

use koe_ffi::AudioSourceConfig;
use oto_encode::Comment;

/// `OpusTags` comment fields written into the identification header.
#[derive(Debug, Clone)]
pub struct OggComments {
    /// `TITLE` tag.
    pub title: String,
    /// `ARTIST` tag.
    pub artist: String,
    /// `DATE` tag (ISO 8601).
    pub date: String,
    /// `DESCRIPTION` tag.
    pub description: String,
    /// `ENCODER` tag.
    pub encoder: String,
    /// `KOE_SOURCE` tag (JSON of the capture source).
    pub koe_source: String,
}

impl OggComments {
    /// Minimal tags when session metadata is unavailable.
    #[must_use]
    pub fn basic() -> Self {
        Self {
            title: "Koe recording".to_owned(),
            artist: "Koe".to_owned(),
            date: String::new(),
            description: String::new(),
            encoder: format!("koe v{}", env!("CARGO_PKG_VERSION")),
            koe_source: r#"{"type":"unknown"}"#.to_owned(),
        }
    }

    /// Builds tags for a recording session from capture source and locale.
    #[must_use]
    pub fn for_session(
        source: &AudioSourceConfig,
        locale: &str,
    ) -> Self {
        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let (date, time, iso) = unix_to_civil(now_secs);
        let app_name = source_label(source);
        Self {
            title: format!("{app_name} recording — {date} {time}"),
            artist: "Koe".to_owned(),
            date: iso,
            description: format!("Source: {app_name}, Locale: {locale}"),
            encoder: format!("koe v{}", env!("CARGO_PKG_VERSION")),
            koe_source: source_json(source),
        }
    }

    /// Returns the tags as `KEY=VALUE` comments for the shared encoder.
    ///
    /// The element type is [`oto_encode::Comment`], the input of
    /// [`oto_encode::OggEncoder::new`]. Most koe callers should use
    /// [`create_encoder`](crate::codec::create_encoder) instead, which builds
    /// the encoder (and tags) for them.
    #[must_use]
    pub fn as_comments(&self) -> Vec<Comment> {
        let pairs = self.as_pairs();
        pairs
            .into_iter()
            .map(|(key, value)| Comment {
                key: key.to_owned(),
                value: value.to_owned(),
            })
            .collect()
    }

    const fn as_pairs(&self) -> [(&str, &str); 6] {
        [
            ("TITLE", self.title.as_str()),
            ("ARTIST", self.artist.as_str()),
            ("DATE", self.date.as_str()),
            ("DESCRIPTION", self.description.as_str()),
            ("ENCODER", self.encoder.as_str()),
            ("KOE_SOURCE", self.koe_source.as_str()),
        ]
    }
}

fn source_label(source: &AudioSourceConfig) -> String {
    match source {
        AudioSourceConfig::AppAudio { bundle_id } | AudioSourceConfig::Both { bundle_id } => {
            bundle_id.clone()
        },
        AudioSourceConfig::PidAudio { pid } => format!("pid:{pid}"),
        AudioSourceConfig::Microphone => "Microphone".to_owned(),
    }
}

fn source_json(source: &AudioSourceConfig) -> String {
    match source {
        AudioSourceConfig::AppAudio { bundle_id } => {
            format!(
                r#"{{"type":"app_audio","bundle_id":"{}"}}"#,
                escape_json(bundle_id)
            )
        },
        AudioSourceConfig::PidAudio { pid } => {
            format!(r#"{{"type":"pid_audio","pid":{pid}}}"#)
        },
        AudioSourceConfig::Microphone => r#"{"type":"microphone"}"#.to_owned(),
        AudioSourceConfig::Both { bundle_id } => {
            format!(
                r#"{{"type":"both","bundle_id":"{}"}}"#,
                escape_json(bundle_id)
            )
        },
    }
}

fn escape_json(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            },
            c => out.push(c),
        }
    }
    out
}

/// Converts Unix seconds to `(YYYY-MM-DD, HH:MM:SS, ISO-8601Z)` in UTC.
fn unix_to_civil(secs: u64) -> (String, String, String) {
    let days = i32::try_from(secs / 86_400).unwrap_or(i32::MAX);
    let tod = secs % 86_400;
    let (year, month, day) = civil_from_days(days);
    let hour = tod / 3_600;
    let minute = (tod % 3_600) / 60;
    let second = tod % 60;
    let date = format!("{year:04}-{month:02}-{day:02}");
    let time = format!("{hour:02}:{minute:02}:{second:02}");
    let iso = format!("{date}T{time}Z");
    (date, time, iso)
}

/// Howard Hinnant's `civil_from_days` (days since 1970-01-01 → y/m/d).
fn civil_from_days(days_since_epoch: i32) -> (i32, u32, u32) {
    let z = days_since_epoch + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = u32::try_from(z - era * 146_097).unwrap_or(0);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = i32::try_from(yoe).unwrap_or(0) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    (year, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comments_are_key_value_pairs() {
        let comments = OggComments {
            title: "t".into(),
            artist: "a".into(),
            date: "2026-08-11T12:00:00Z".into(),
            description: "d".into(),
            encoder: "koe v0.0.0".into(),
            koe_source: r#"{"type":"microphone"}"#.into(),
        };
        let pairs = comments.as_comments();
        assert_eq!(pairs.len(), 6);
        assert_eq!(pairs[0].key, "TITLE");
        assert_eq!(pairs[0].value, "t");
        assert_eq!(pairs[4].key, "ENCODER");
        assert_eq!(pairs[4].value, "koe v0.0.0");
    }

    #[test]
    fn civil_from_epoch() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        // 2024-02-29 (leap day): 19782 days after epoch.
        assert_eq!(civil_from_days(19_782), (2024, 2, 29));
        assert_eq!(civil_from_days(20_161), (2025, 3, 14));
        assert_eq!(
            unix_to_civil(0),
            (
                "1970-01-01".to_owned(),
                "00:00:00".to_owned(),
                "1970-01-01T00:00:00Z".to_owned(),
            )
        );
    }
}

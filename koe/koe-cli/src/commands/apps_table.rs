//! Shared capture-source table / JSON formatting for `koe list` and
//! `koe record --list-sources`.

use std::fmt::Write as _;

use koe_core::AppInfo;
use serde_json::{Value, json};

use crate::MainError;

/// Filter and stably order apps for display.
pub fn prepare_apps(
    apps: Vec<AppInfo>,
    audio_only: bool,
) -> Vec<AppInfo> {
    let mut apps = if audio_only {
        apps.into_iter().filter(|app| app.has_audio).collect()
    } else {
        apps
    };
    sort_apps(&mut apps);
    apps
}

/// Active-audio apps first, then name (ASCII case-insensitive), then PID.
fn sort_apps(apps: &mut [AppInfo]) {
    apps.sort_by(|a, b| {
        b.has_audio
            .cmp(&a.has_audio)
            .then_with(|| cmp_ascii_ignore_case(&a.name, &b.name))
            .then_with(|| a.pid.cmp(&b.pid))
    });
}

fn cmp_ascii_ignore_case(
    left: &str,
    right: &str,
) -> std::cmp::Ordering {
    left.chars()
        .map(|c| c.to_ascii_lowercase())
        .cmp(right.chars().map(|c| c.to_ascii_lowercase()))
}

pub fn format_apps_table(apps: &[AppInfo]) -> String {
    let mut out = String::from(
        "  PID    NAME                  BUNDLE ID               HAS AUDIO\n  ─────  ────────────────────  ──────────────────────  ─────────\n",
    );
    for app in apps {
        let name = sanitize_for_table(&app.name);
        let bundle = app
            .bundle_id
            .as_deref()
            .map_or_else(|| "-".to_owned(), sanitize_for_table);
        let has_audio = if app.has_audio { "yes" } else { "no" };
        let _ = writeln!(
            out,
            "  {:<5}  {:<20}  {:<22}  {}",
            app.pid,
            truncate(&name, 20),
            truncate(&bundle, 22),
            has_audio
        );
    }
    out
}

pub fn format_apps_json(apps: &[AppInfo]) -> Result<String, MainError> {
    let rows: Vec<Value> = apps
        .iter()
        .map(|app| {
            json!({
                "pid": app.pid,
                "name": app.name,
                "bundle_id": app.bundle_id,
                "has_audio": app.has_audio,
            })
        })
        .collect();
    Ok(serde_json::to_string_pretty(&rows)?)
}

/// Strip C0/C1 control characters so table layout cannot be broken.
fn sanitize_for_table(value: &str) -> String {
    value.chars().filter(|c| !c.is_control()).collect()
}

fn truncate(
    value: &str,
    max: usize,
) -> String {
    if value.chars().count() <= max {
        return value.to_owned();
    }
    let mut truncated: String = value.chars().take(max.saturating_sub(1)).collect();
    truncated.push('…');
    truncated
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_apps() -> Vec<AppInfo> {
        vec![
            AppInfo {
                pid: 1234,
                name: "Finder".into(),
                bundle_id: Some("com.apple.Finder".into()),
                has_audio: false,
            },
            AppInfo {
                pid: 8891,
                name: "Spotify".into(),
                bundle_id: Some("com.spotify.client".into()),
                has_audio: true,
            },
            AppInfo {
                pid: 4201,
                name: "Google Chrome".into(),
                bundle_id: Some("com.google.Chrome".into()),
                has_audio: true,
            },
            AppInfo {
                pid: 99,
                name: "Helper".into(),
                bundle_id: None,
                has_audio: false,
            },
        ]
    }

    #[test]
    fn audio_only_filters_silent_apps() {
        let filtered = prepare_apps(sample_apps(), true);
        assert_eq!(filtered.len(), 2);
        assert!(filtered.iter().all(|app| app.has_audio));
    }

    #[test]
    fn audio_only_false_keeps_all() {
        assert_eq!(prepare_apps(sample_apps(), false).len(), 4);
    }

    #[test]
    fn audio_only_empty_input_stays_empty() {
        assert!(prepare_apps(Vec::new(), true).is_empty());
    }

    #[test]
    fn sort_puts_audio_apps_first_then_name() {
        let prepared = prepare_apps(sample_apps(), false);
        let names: Vec<&str> = prepared.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["Google Chrome", "Spotify", "Finder", "Helper"]);
    }

    #[test]
    fn sort_is_ascii_case_insensitive_on_name() {
        let apps = vec![
            AppInfo {
                pid: 2,
                name: "beta".into(),
                bundle_id: None,
                has_audio: true,
            },
            AppInfo {
                pid: 1,
                name: "Alpha".into(),
                bundle_id: None,
                has_audio: true,
            },
        ];
        let prepared = prepare_apps(apps, false);
        assert_eq!(prepared[0].name, "Alpha");
        assert_eq!(prepared[1].name, "beta");
    }

    #[test]
    fn table_includes_header_and_rows() {
        let table = format_apps_table(&prepare_apps(sample_apps(), false));
        assert!(table.contains("PID"));
        assert!(table.contains("Google Chrome"));
        assert!(table.contains("yes"));
        assert!(table.contains("no"));
        assert!(table.contains("Helper"));
        assert!(table.contains("  99     Helper"));
        assert!(table.contains('-'));
    }

    #[test]
    fn table_strips_control_chars_and_truncates() {
        let apps = vec![AppInfo {
            pid: 1,
            name: "Bad\nName\x07WithControls".into(),
            bundle_id: Some("com.example.very.long.bundle.id.that.overflows".into()),
            has_audio: false,
        }];
        let table = format_apps_table(&apps);
        // header + rule + one data line
        assert_eq!(table.lines().count(), 3);
        assert!(!table.contains('\x07'));
        assert!(table.contains("BadNameWithControls") || table.contains("BadNameWithControl…"));
        assert!(table.contains('…'));
    }

    #[test]
    fn truncate_long_names() {
        assert_eq!(truncate("abcdefghij", 5), "abcd…");
        assert_eq!(truncate("short", 10), "short");
    }

    #[test]
    fn json_is_array_of_objects() {
        let json = format_apps_json(&prepare_apps(sample_apps(), false)).expect("json");
        let value: Value = serde_json::from_str(&json).expect("parse");
        let arr = value.as_array().expect("array");
        assert_eq!(arr.len(), 4);
        assert_eq!(arr[0]["pid"], 4201);
        assert_eq!(arr[0]["has_audio"], true);
        assert_eq!(arr[1]["name"], "Spotify");
        assert_eq!(arr[2]["bundle_id"], "com.apple.Finder");
        assert!(arr[3]["bundle_id"].is_null());
    }
}

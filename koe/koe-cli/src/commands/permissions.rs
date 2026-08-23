//! `koe permissions` — diagnose TCC permission status.

use std::fmt::Write as _;

use koe_core::{Permission, PermissionStatus, check_permission, native_provider_registered};
use serde_json::{Value, json};
use usage::Args;

use super::Run;
use crate::MainError;

/// Check and diagnose macOS permissions required by Koe.
#[derive(Debug, Args)]
pub struct PermissionsArgs {
    /// Output as JSON.
    #[usage(long)]
    json: bool,

    /// Exit non-zero if any permission is not Authorized.
    #[usage(long)]
    check: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PermissionRow {
    permission: Permission,
    status: PermissionStatus,
}

impl Run for PermissionsArgs {
    fn run(
        self,
        _config: &crate::config::KoeConfig,
    ) -> Result<(), MainError> {
        if !native_provider_registered() {
            return Err(MainError::NativeBridgeUnavailable("permissions"));
        }

        let rows = collect_permissions();
        if self.json {
            println!("{}", format_permissions_json(&rows)?);
        } else {
            print!("{}", format_permissions_table(&rows));
        }

        if self.check
            && rows
                .iter()
                .any(|row| !matches!(row.status, PermissionStatus::Authorized))
        {
            return Err(MainError::PermissionsNotAuthorized);
        }
        Ok(())
    }
}

fn collect_permissions() -> Vec<PermissionRow> {
    [
        Permission::Microphone,
        Permission::ScreenRecording,
        Permission::Accessibility,
    ]
    .into_iter()
    .map(|permission| PermissionRow {
        permission,
        status: check_permission(permission),
    })
    .collect()
}

fn format_permissions_table(rows: &[PermissionRow]) -> String {
    let mut out = String::from(
        "  PERMISSION          STATUS           FIX\n  ─────────────────   ──────────────   ──────────────────────────────────────\n",
    );
    for row in rows {
        let fix = fix_hint(row.permission, row.status).unwrap_or("");
        let _ = writeln!(
            out,
            "  {:<19} {:<16} {}",
            permission_label(row.permission),
            status_label(row.status),
            fix
        );
    }

    if rows
        .iter()
        .any(|row| !matches!(row.status, PermissionStatus::Authorized))
    {
        out.push_str(
            "\nIf the terminal has permissions issues:\n  Note: Permissions for \"Terminal.app\" differ from \"Koe.app\" (GUI).\n  The GUI handles permission prompts automatically.\n",
        );
    }
    out
}

fn format_permissions_json(rows: &[PermissionRow]) -> Result<String, MainError> {
    let payload: Vec<Value> = rows
        .iter()
        .map(|row| {
            json!({
                "permission": permission_key(row.permission),
                "status": status_key(row.status),
                "fix": fix_hint(row.permission, row.status),
            })
        })
        .collect();
    Ok(serde_json::to_string_pretty(&payload)?)
}

const fn permission_label(permission: Permission) -> &'static str {
    match permission {
        Permission::Microphone => "Microphone",
        Permission::ScreenRecording => "Screen Recording",
        Permission::Accessibility => "Accessibility",
    }
}

const fn permission_key(permission: Permission) -> &'static str {
    match permission {
        Permission::Microphone => "microphone",
        Permission::ScreenRecording => "screen_recording",
        Permission::Accessibility => "accessibility",
    }
}

const fn status_label(status: PermissionStatus) -> &'static str {
    match status {
        PermissionStatus::Authorized => "Authorized",
        PermissionStatus::Denied => "Denied",
        PermissionStatus::Restricted => "Restricted",
        PermissionStatus::NotDetermined => "NotDetermined",
    }
}

const fn status_key(status: PermissionStatus) -> &'static str {
    match status {
        PermissionStatus::Authorized => "authorized",
        PermissionStatus::Denied => "denied",
        PermissionStatus::Restricted => "restricted",
        PermissionStatus::NotDetermined => "not_determined",
    }
}

const fn fix_hint(
    permission: Permission,
    status: PermissionStatus,
) -> Option<&'static str> {
    match status {
        PermissionStatus::Authorized => None,
        PermissionStatus::Denied => Some(match permission {
            Permission::Microphone => "Open System Settings → Privacy & Security → Microphone",
            Permission::ScreenRecording => {
                "Open System Settings → Privacy & Security → Screen Recording"
            },
            Permission::Accessibility => {
                "Open System Settings → Privacy & Security → Accessibility"
            },
        }),
        PermissionStatus::NotDetermined => {
            Some("Not prompted yet — run the Koe GUI once so macOS can show the permission dialog")
        },
        PermissionStatus::Restricted => Some("Restricted by MDM or parental controls"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_rows() -> Vec<PermissionRow> {
        vec![
            PermissionRow {
                permission: Permission::Microphone,
                status: PermissionStatus::Authorized,
            },
            PermissionRow {
                permission: Permission::ScreenRecording,
                status: PermissionStatus::Denied,
            },
            PermissionRow {
                permission: Permission::Accessibility,
                status: PermissionStatus::Denied,
            },
        ]
    }

    #[test]
    fn table_includes_fix_for_denied() {
        let table = format_permissions_table(&sample_rows());
        assert!(table.contains("Microphone"));
        assert!(table.contains("Authorized"));
        assert!(table.contains("Screen Recording"));
        assert!(table.contains("Open System Settings → Privacy & Security → Screen Recording"));
        assert!(table.contains("Open System Settings → Privacy & Security → Accessibility"));
        assert!(table.contains("Terminal.app"));
    }

    #[test]
    fn table_status_specific_hints() {
        let rows = vec![
            PermissionRow {
                permission: Permission::Microphone,
                status: PermissionStatus::NotDetermined,
            },
            PermissionRow {
                permission: Permission::ScreenRecording,
                status: PermissionStatus::Restricted,
            },
        ];
        let table = format_permissions_table(&rows);
        assert!(table.contains("NotDetermined"));
        assert!(table.contains("Restricted"));
        assert!(table.contains("Not prompted yet"));
        assert!(table.contains("MDM or parental controls"));
        // STATUS column widened enough that NotDetermined is not truncated mid-token
        assert!(table.contains("NotDetermined   "));
    }

    #[test]
    fn json_uses_snake_case_keys() {
        let json = format_permissions_json(&sample_rows()).expect("json");
        let value: Value = serde_json::from_str(&json).expect("parse");
        let arr = value.as_array().expect("array");
        assert_eq!(arr.len(), 3);
        assert_eq!(
            arr[0],
            json!({
                "permission": "microphone",
                "status": "authorized",
                "fix": null,
            })
        );
        assert_eq!(
            arr[1],
            json!({
                "permission": "screen_recording",
                "status": "denied",
                "fix": "Open System Settings → Privacy & Security → Screen Recording",
            })
        );
        assert_eq!(
            arr[2],
            json!({
                "permission": "accessibility",
                "status": "denied",
                "fix": "Open System Settings → Privacy & Security → Accessibility",
            })
        );
    }

    #[test]
    fn run_errors_without_native_provider() {
        let err = PermissionsArgs {
            json: false,
            check: false,
        }
        .run(&crate::config::KoeConfig::default())
        .expect_err("must fail without provider");
        assert!(matches!(
            err,
            MainError::NativeBridgeUnavailable("permissions")
        ));
    }
}

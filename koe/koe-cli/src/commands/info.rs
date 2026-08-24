//! `koe info` — system / build diagnostics.

use std::env::consts::{ARCH, OS};
use std::fmt::Write as _;
use std::path::PathBuf;
use std::process::Command;

use koe_core::{
    AudioDeviceInfo, available_disk_space, default_input_device, default_output_device,
    enabled_features, supported_speech_locales,
};
use serde_json::json;
use usage::Args;

use super::Run;
use crate::MainError;
use crate::config::{self, KoeConfig};

/// Prefer `[output].directory` from config; otherwise `~/Movies` or `.`.
fn disk_check_path(config: &KoeConfig) -> PathBuf {
    if let Some(dir) = config::output_directory(config) {
        return dir;
    }
    disk_check_path_from_home(std::env::var_os("HOME").as_deref())
}

fn disk_check_path_from_home(home: Option<&std::ffi::OsStr>) -> PathBuf {
    home.map_or_else(|| PathBuf::from("."), |h| PathBuf::from(h).join("Movies"))
}

/// Show build and host system information.
#[derive(Debug, Args)]
pub struct InfoArgs {
    /// Output as JSON.
    #[usage(long)]
    json: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SystemInfo {
    version: String,
    /// Host architecture and OS (`std::env::consts`), not a rustc target triple.
    host: String,
    features: Vec<&'static str>,
    macos_version: Option<String>,
    disk_check_path: String,
    disk_space_bytes: Option<u64>,
    default_input_device: Option<AudioDeviceInfo>,
    default_output_device: Option<AudioDeviceInfo>,
    supported_locales: Vec<String>,
}

impl Run for InfoArgs {
    fn run(
        self,
        config: &KoeConfig,
    ) -> Result<(), MainError> {
        let info = collect_system_info(config);
        if self.json {
            println!("{}", format_info_json(&info)?);
        } else {
            print!("{}", format_info_text(&info));
        }
        Ok(())
    }
}

fn collect_system_info(config: &KoeConfig) -> SystemInfo {
    let disk_path = disk_check_path(config);
    let disk_check_path = disk_path.display().to_string();
    let disk_space_bytes = match available_disk_space(&disk_path) {
        Ok(bytes) => Some(bytes),
        Err(err) => {
            eprintln!("warning: could not query disk space at {disk_check_path}: {err}");
            None
        },
    };

    SystemInfo {
        version: env!("CARGO_PKG_VERSION").to_owned(),
        host: format!("{ARCH}-{OS}"),
        features: enabled_features(),
        macos_version: macos_version(),
        disk_check_path,
        disk_space_bytes,
        default_input_device: default_input_device(),
        default_output_device: default_output_device(),
        supported_locales: supported_speech_locales(),
    }
}

fn macos_version() -> Option<String> {
    if OS != "macos" {
        return None;
    }
    let output = Command::new("/usr/bin/sw_vers")
        .arg("-productVersion")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let version = String::from_utf8(output.stdout).ok()?;
    let version = version.trim();
    if is_version_like(version) {
        Some(version.to_owned())
    } else {
        None
    }
}

/// Accept only printable ASCII version-like text (digits and `.`).
fn is_version_like(value: &str) -> bool {
    !value.is_empty()
        && value.chars().all(|c| c.is_ascii_digit() || c == '.')
        && value.chars().any(|c| c.is_ascii_digit())
}

fn format_info_text(info: &SystemInfo) -> String {
    let mut out = format!(
        "  Koe version:     {}\n  Host (arch-os):  {}\n  Feature flags:   {}\n",
        info.version,
        info.host,
        info.features.join(", ")
    );
    if let Some(version) = &info.macos_version {
        let _ = writeln!(out, "  macOS version:   {version}");
    }
    format_device_line(
        &mut out,
        "Default input:",
        info.default_input_device.as_ref(),
    );
    format_device_line(
        &mut out,
        "Default output:",
        info.default_output_device.as_ref(),
    );
    match info.disk_space_bytes {
        Some(bytes) => {
            let _ = writeln!(
                out,
                "  Disk space:      {} free ({})",
                format_bytes(bytes),
                info.disk_check_path
            );
        },
        None => {
            let _ = writeln!(
                out,
                "  Disk space:      unavailable ({})",
                info.disk_check_path
            );
        },
    }
    if info.supported_locales.is_empty() {
        out.push_str("  Locales:         (none reported)\n");
    } else {
        let _ = writeln!(
            out,
            "  Locales:         {}",
            info.supported_locales.join(", ")
        );
    }
    out
}

fn format_device_line(
    out: &mut String,
    label: &str,
    device: Option<&AudioDeviceInfo>,
) {
    match device {
        Some(device) => {
            let _ = writeln!(out, "  {label:<16} {device}");
        },
        None => {
            let _ = writeln!(out, "  {label:<16} unavailable");
        },
    }
}

fn format_info_json(info: &SystemInfo) -> Result<String, MainError> {
    let payload = json!({
        "version": info.version,
        "host": info.host,
        "features": info.features,
        "macos_version": info.macos_version,
        "disk_check_path": info.disk_check_path,
        "disk_space_bytes": info.disk_space_bytes,
        "default_input_device": info.default_input_device.as_ref().map(device_json),
        "default_output_device": info.default_output_device.as_ref().map(device_json),
        "supported_locales": info.supported_locales,
    });
    Ok(serde_json::to_string_pretty(&payload)?)
}

fn device_json(device: &AudioDeviceInfo) -> serde_json::Value {
    json!({
        "name": device.name,
        "uid": device.uid,
    })
}

fn format_bytes(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    const GIB: u64 = MIB * 1024;

    if bytes >= GIB {
        format!("{}.{} GiB", bytes / GIB, (bytes % GIB) * 10 / GIB)
    } else if bytes >= MIB {
        format!("{}.{} MiB", bytes / MIB, (bytes % MIB) * 10 / MIB)
    } else if bytes >= KIB {
        format!("{}.{} KiB", bytes / KIB, (bytes % KIB) * 10 / KIB)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn sample_info() -> SystemInfo {
        SystemInfo {
            version: "0.0.0".into(),
            host: "aarch64-macos".into(),
            features: vec!["aec", "cli", "screen-audio", "system-audio"],
            macos_version: Some("15.5.0".into()),
            disk_check_path: "/Users/test/Movies".into(),
            disk_space_bytes: Some(5 * 1024 * 1024 * 1024),
            default_input_device: Some(AudioDeviceInfo {
                name: "MacBook Pro Microphone".into(),
                uid: "BuiltInMicrophoneDevice".into(),
            }),
            default_output_device: Some(AudioDeviceInfo {
                name: "MacBook Pro Speakers".into(),
                uid: "BuiltInSpeakerDevice".into(),
            }),
            supported_locales: vec!["en-US".into(), "ja-JP".into()],
        }
    }

    fn sparse_info() -> SystemInfo {
        SystemInfo {
            version: "0.0.0".into(),
            host: "aarch64-linux".into(),
            features: vec!["cli"],
            macos_version: None,
            disk_check_path: "/tmp".into(),
            disk_space_bytes: None,
            default_input_device: None,
            default_output_device: None,
            supported_locales: vec![],
        }
    }

    #[test]
    fn text_includes_core_fields() {
        let text = format_info_text(&sample_info());
        assert!(text.contains("0.0.0"));
        assert!(text.contains("Host (arch-os):  aarch64-macos"));
        assert!(text.contains("aec, cli, screen-audio, system-audio"));
        assert!(text.contains("15.5.0"));
        assert!(text.contains("5.0 GiB"));
        assert!(text.contains("/Users/test/Movies"));
        assert!(text.contains("MacBook Pro Microphone"));
        assert!(text.contains("BuiltInMicrophoneDevice"));
        assert!(text.contains("MacBook Pro Speakers"));
        assert!(text.contains("BuiltInSpeakerDevice"));
        assert!(text.contains("en-US, ja-JP"));
        assert!(!text.contains("NativeProvider"));
        assert!(!text.contains("tasks beyond"));
    }

    #[test]
    fn text_handles_missing_optional_fields() {
        let text = format_info_text(&sparse_info());
        assert!(!text.contains("macOS version:"));
        assert!(text.contains("Disk space:      unavailable (/tmp)"));
        assert!(text.contains("Default input:   unavailable"));
        assert!(text.contains("Default output:  unavailable"));
        assert!(text.contains("Locales:         (none reported)"));
    }

    #[test]
    fn json_includes_devices_and_locales() {
        let json = format_info_json(&sample_info()).expect("json");
        let value: serde_json::Value = serde_json::from_str(&json).expect("parse");
        assert_eq!(value["version"], "0.0.0");
        assert_eq!(value["host"], "aarch64-macos");
        assert_eq!(value["macos_version"], "15.5.0");
        assert_eq!(value["disk_check_path"], "/Users/test/Movies");
        assert_eq!(value["disk_space_bytes"], 5_368_709_120_u64);
        assert_eq!(
            value["default_input_device"],
            json!({"name": "MacBook Pro Microphone", "uid": "BuiltInMicrophoneDevice"})
        );
        assert_eq!(
            value["default_output_device"],
            json!({"name": "MacBook Pro Speakers", "uid": "BuiltInSpeakerDevice"})
        );
        assert_eq!(value["supported_locales"], json!(["en-US", "ja-JP"]));
        assert!(
            value["features"]
                .as_array()
                .expect("features")
                .contains(&json!("aec"))
        );
    }

    #[test]
    fn json_null_optional_fields() {
        let json = format_info_json(&sparse_info()).expect("json");
        let value: serde_json::Value = serde_json::from_str(&json).expect("parse");
        assert!(value["macos_version"].is_null());
        assert!(value["disk_space_bytes"].is_null());
        assert_eq!(value["disk_check_path"], "/tmp");
        assert!(value["default_input_device"].is_null());
        assert!(value["default_output_device"].is_null());
        assert_eq!(value["supported_locales"], json!([]));
    }

    #[test]
    fn format_bytes_scales() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(2048), "2.0 KiB");
        assert_eq!(format_bytes(3 * 1024 * 1024), "3.0 MiB");
        assert_eq!(format_bytes(1536), "1.5 KiB");
        assert_eq!(format_bytes(5 * 1024 * 1024 + 512 * 1024), "5.5 MiB");
    }

    #[test]
    fn version_like_rejects_noise() {
        assert!(is_version_like("15.5.0"));
        assert!(is_version_like("14"));
        assert!(!is_version_like(""));
        assert!(!is_version_like("15.5.0\nextra"));
        assert!(!is_version_like("not-a-version"));
        assert!(!is_version_like("15.5.0; rm -rf /"));
    }

    #[test]
    fn disk_path_uses_home_movies_when_set() {
        let path = disk_check_path_from_home(Some(std::ffi::OsStr::new("/tmp/koe-info-home")));
        assert_eq!(path, Path::new("/tmp/koe-info-home/Movies"));
        assert_eq!(disk_check_path_from_home(None), PathBuf::from("."));
    }

    #[test]
    fn disk_path_prefers_config_output_directory() {
        let mut config = KoeConfig::default();
        config.output.directory = Some("/tmp/koe-configured-out".into());
        assert_eq!(
            disk_check_path(&config),
            PathBuf::from("/tmp/koe-configured-out")
        );
    }
}

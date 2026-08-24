//! `oto list` — enumerate input audio devices.

use std::fmt::Write as _;

use oto_core::{DeviceInfo, list_input_devices};
use usage::Args;

use super::Run;
use crate::MainError;

/// List input audio devices.
#[derive(Debug, Args)]
pub struct ListArgs {
    /// Output the device list as a JSON array.
    #[usage(long)]
    json: bool,
}

impl Run for ListArgs {
    fn run(self) -> Result<(), MainError> {
        let devices = list_input_devices().map_err(|e| MainError::Capture(e.to_string()))?;
        if devices.is_empty() {
            return Err(MainError::Capture(
                "no input devices found (check microphone connections and permissions)".to_owned(),
            ));
        }
        if self.json {
            let json =
                serde_json::to_string(&devices).map_err(|e| MainError::Internal(e.to_string()))?;
            println!("{json}");
        } else {
            print!("{}", render_list(&devices));
        }
        Ok(())
    }
}

/// Renders the device list as human-readable rows.
fn render_list(devices: &[DeviceInfo]) -> String {
    let mut out = String::new();
    for (index, device) in devices.iter().enumerate() {
        let position = index + 1;
        let _ = writeln!(
            out,
            "{position}: {} (ID: {}) {} {} Hz",
            device.name,
            device.unique_id,
            channel_label(device.channels),
            device.sample_rate
        );
    }
    out
}

fn channel_label(channels: i32) -> String {
    match channels {
        1 => "mono".to_owned(),
        2 => "stereo".to_owned(),
        n => format!("{n}ch"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_devices() -> Vec<DeviceInfo> {
        vec![
            DeviceInfo {
                name: "MacBook Pro Microphone".to_owned(),
                unique_id: "8D93D0E0-AB12".to_owned(),
                channels: 1,
                sample_rate: 48_000,
            },
            DeviceInfo {
                name: "USB Audio Device".to_owned(),
                unique_id: "USB-1".to_owned(),
                channels: 2,
                sample_rate: 44_100,
            },
        ]
    }

    #[test]
    fn renders_human_readable_rows() {
        assert_eq!(
            render_list(&sample_devices()),
            "1: MacBook Pro Microphone (ID: 8D93D0E0-AB12) mono 48000 Hz\n\
             2: USB Audio Device (ID: USB-1) stereo 44100 Hz\n"
        );
    }

    #[test]
    fn renders_empty_list_as_no_output() {
        assert_eq!(render_list(&[]), "");
    }

    #[test]
    fn labels_channels() {
        assert_eq!(channel_label(1), "mono");
        assert_eq!(channel_label(2), "stereo");
        assert_eq!(channel_label(3), "3ch");
    }

    #[test]
    fn serializes_devices_as_json() {
        let json = serde_json::to_string(&sample_devices()).unwrap();
        assert_eq!(
            json,
            r#"[{"name":"MacBook Pro Microphone","unique_id":"8D93D0E0-AB12","channels":1,"sample_rate":48000},{"name":"USB Audio Device","unique_id":"USB-1","channels":2,"sample_rate":44100}]"#
        );
    }
}

//! Shared fixtures for pipeline unit tests.

use std::path::{Path, PathBuf};

use koe_ffi::{
    AppInfo, NativeProvider, OutputFormat, Permission, PermissionStatus, TranscriptFormat,
    register_native_provider,
};

use super::PipelineConfig;

pub struct TestProvider {
    permissions: Vec<(Permission, PermissionStatus)>,
}

impl NativeProvider for TestProvider {
    fn check_permission(
        &self,
        permission: Permission,
    ) -> PermissionStatus {
        self.permissions
            .iter()
            .find(|(perm, _)| *perm == permission)
            .map_or(PermissionStatus::NotDetermined, |(_, status)| *status)
    }

    fn request_permission(
        &self,
        permission: Permission,
    ) -> PermissionStatus {
        self.check_permission(permission)
    }

    fn enumerate_apps(&self) -> Vec<AppInfo> {
        Vec::new()
    }
}

pub fn install_provider(permissions: Vec<(Permission, PermissionStatus)>) {
    koe_ffi::set_capture_stub(true);
    koe_ffi::set_transcription_stub(true);
    register_native_provider(Box::new(TestProvider { permissions }));
}

pub fn install_authorized_mic() {
    install_provider(vec![(Permission::Microphone, PermissionStatus::Authorized)]);
}

pub fn unique_path(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "koe-pipeline-{label}-{}-{}.wav",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ))
}

pub fn test_config(output: &Path) -> PipelineConfig {
    PipelineConfig {
        source: koe_ffi::AudioSourceConfig::Microphone,
        output_path: output.to_path_buf(),
        transcript_output_path: None,
        locale: "en-US".into(),
        speech_engine: koe_ffi::SpeechEngine::Auto,
        audio_format: OutputFormat::Wav {
            bits_per_sample: 16,
        },
        transcript_format: TranscriptFormat::Txt,
        enable_aec: false,
        comfort_noise: false,
        monitor: false,
        transcribe: true,
        estimated_duration_hours: None,
    }
}

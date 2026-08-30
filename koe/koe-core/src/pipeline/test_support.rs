//! Shared fixtures for pipeline unit tests.

use std::path::{Path, PathBuf};

use koe_ffi::{
    AppInfo, NativeProvider, OutputFormat, Permission, PermissionStatus, TranscriptFormat,
    register_native_provider,
};
use tokio::sync::{Mutex, MutexGuard};

use super::PipelineConfig;

/// Installs a [`NativeProvider`] and returns a guard that must be held for the
/// whole test body.
///
/// `koe-ffi` keeps the provider in a single process-wide slot, so tests that
/// install different permissions must not run concurrently or one test's
/// provider can be observed by another. Folding the guard into the install
/// makes it impossible to introduce that race: acquiring the guard is the only
/// way to install.
pub async fn install_provider(
    permissions: Vec<(Permission, PermissionStatus)>
) -> MutexGuard<'static, ()> {
    static PROVIDER_LOCK: Mutex<()> = Mutex::const_new(());
    let guard = PROVIDER_LOCK.lock().await;
    koe_ffi::set_capture_stub(true);
    koe_ffi::set_transcription_stub(true);
    register_native_provider(Box::new(TestProvider { permissions }));
    guard
}

pub async fn install_authorized_mic() -> MutexGuard<'static, ()> {
    install_provider(vec![(Permission::Microphone, PermissionStatus::Authorized)]).await
}

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

pub fn unique_path(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "koe-pipeline-{label}-{}-{}.ogg",
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
        audio_format: OutputFormat::Ogg { bitrate_bps: None },
        transcript_format: TranscriptFormat::Txt,
        enable_aec: false,
        comfort_noise: false,
        monitor: false,
        transcribe: true,
        estimated_duration_hours: None,
    }
}

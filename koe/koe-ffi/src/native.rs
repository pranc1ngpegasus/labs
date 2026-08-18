//! Swift-side native provider registration and delegation.

use std::sync::{Arc, RwLock};

use crate::types::{AppInfo, Permission, PermissionStatus};

/// macOS framework bridge implemented by `koe-native` on the Swift side.
#[uniffi::export(callback_interface)]
pub trait NativeProvider: Send + Sync {
    fn check_permission(
        &self,
        permission: Permission,
    ) -> PermissionStatus;
    fn request_permission(
        &self,
        permission: Permission,
    ) -> PermissionStatus;
    fn enumerate_apps(&self) -> Vec<AppInfo>;
}

static NATIVE_PROVIDER: RwLock<Option<Arc<dyn NativeProvider>>> = RwLock::new(None);

/// Registers the Swift implementation of macOS framework calls.
///
/// Must be called once before any other FFI entry point that touches native
/// APIs. Later registrations replace the previous provider (used in tests).
#[uniffi::export]
pub fn register_native_provider(provider: Box<dyn NativeProvider>) {
    if let Ok(mut guard) = NATIVE_PROVIDER.write() {
        *guard = Some(Arc::from(provider));
    }
}

pub fn provider() -> Option<Arc<dyn NativeProvider>> {
    NATIVE_PROVIDER.read().ok().and_then(|guard| guard.clone())
}

/// Returns whether a [`NativeProvider`] has been registered.
///
/// CLI and other Rust hosts that do not link `koe-native` can probe this
/// before calling [`crate::enumerate_apps`] / [`crate::check_permission`],
/// which otherwise silently degrade to empty / `NotDetermined`.
#[must_use]
pub fn native_provider_registered() -> bool {
    provider().is_some()
}

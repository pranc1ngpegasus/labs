//! macOS discovery provider implemented in Rust.
//!
//! Uses AppKit/AVFoundation via `objc2`, and `CoreAudio` / Accessibility /
//! CoreGraphics via their C APIs. No Objective-C source, no Swift dylib load.

#![allow(unsafe_code)]

use std::sync::mpsc;

use block2::RcBlock;
use objc2::runtime::Bool;
use objc2_app_kit::{NSApplicationActivationPolicy, NSWorkspace};
use objc2_av_foundation::{AVAuthorizationStatus, AVCaptureDevice, AVMediaTypeAudio};
use objc2_foundation::NSString;

use crate::native::{NativeProvider, register_native_provider};
use crate::types::{AppInfo, Permission, PermissionStatus};

type AudioObjectID = u32;
type OSStatus = i32;

#[repr(C)]
struct AudioObjectPropertyAddress {
    selector: u32,
    scope: u32,
    element: u32,
}

const AUDIO_OBJECT_SYSTEM_OBJECT: AudioObjectID = 1;
const AUDIO_OBJECT_UNKNOWN: AudioObjectID = 0;
// Core Audio process-object lookup selector (ptid).
const AUDIO_HARDWARE_TRANSLATE_PID_TO_PROCESS_OBJECT: u32 = u32::from_be_bytes(*b"ptid");

#[link(name = "CoreAudio", kind = "framework")]
unsafe extern "C" {
    fn AudioObjectGetPropertyData(
        object_id: AudioObjectID,
        address: *const AudioObjectPropertyAddress,
        qualifier_data_size: u32,
        qualifier_data: *const std::ffi::c_void,
        data_size: *mut u32,
        data: *mut std::ffi::c_void,
    ) -> OSStatus;
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGPreflightScreenCaptureAccess() -> Bool;
}

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> Bool;
}

struct MacosDiscoveryProvider;

fn ns_to_string(value: Option<&NSString>) -> Option<String> {
    value.map(NSString::to_string)
}

fn has_audio_object(pid: i32) -> bool {
    let address = AudioObjectPropertyAddress {
        selector: AUDIO_HARDWARE_TRANSLATE_PID_TO_PROCESS_OBJECT,
        scope: 0,
        element: 0,
    };
    let mut process_object = AUDIO_OBJECT_UNKNOWN;
    let mut size = u32::try_from(size_of::<AudioObjectID>()).unwrap_or(4);
    let mutable_pid = pid;

    // SAFETY: CoreAudio C ABI; qualifier is a pid_t and out buffer matches size.
    let status = unsafe {
        AudioObjectGetPropertyData(
            AUDIO_OBJECT_SYSTEM_OBJECT,
            &raw const address,
            u32::try_from(size_of_val(&mutable_pid)).unwrap_or(4),
            (&raw const mutable_pid).cast(),
            &raw mut size,
            (&raw mut process_object).cast(),
        )
    };
    status == 0 && process_object != AUDIO_OBJECT_UNKNOWN
}

fn mic_status() -> PermissionStatus {
    let Some(media_type) = (unsafe { AVMediaTypeAudio }) else {
        return PermissionStatus::NotDetermined;
    };
    // SAFETY: documented class method; media type is AVMediaTypeAudio.
    let status = unsafe { AVCaptureDevice::authorizationStatusForMediaType(media_type) };
    match status {
        AVAuthorizationStatus::Authorized => PermissionStatus::Authorized,
        AVAuthorizationStatus::Denied => PermissionStatus::Denied,
        AVAuthorizationStatus::Restricted => PermissionStatus::Restricted,
        _ => PermissionStatus::NotDetermined,
    }
}

fn request_mic() -> PermissionStatus {
    let Some(media_type) = (unsafe { AVMediaTypeAudio }) else {
        return PermissionStatus::NotDetermined;
    };
    let (tx, rx) = mpsc::channel();
    let block = RcBlock::new(move |granted: Bool| {
        let _ = tx.send(granted.as_bool());
    });
    // SAFETY: completion handler is invoked once on an arbitrary queue.
    unsafe {
        AVCaptureDevice::requestAccessForMediaType_completionHandler(media_type, &block);
    }
    if rx.recv().unwrap_or(false) {
        PermissionStatus::Authorized
    } else {
        PermissionStatus::Denied
    }
}

impl NativeProvider for MacosDiscoveryProvider {
    fn check_permission(
        &self,
        permission: Permission,
    ) -> PermissionStatus {
        match permission {
            Permission::Microphone => mic_status(),
            Permission::ScreenRecording => {
                // SAFETY: CoreGraphics C ABI.
                if unsafe { CGPreflightScreenCaptureAccess() }.as_bool() {
                    PermissionStatus::Authorized
                } else {
                    PermissionStatus::Denied
                }
            },
            Permission::Accessibility => {
                // SAFETY: ApplicationServices C ABI.
                if unsafe { AXIsProcessTrusted() }.as_bool() {
                    PermissionStatus::Authorized
                } else {
                    PermissionStatus::Denied
                }
            },
        }
    }

    fn request_permission(
        &self,
        permission: Permission,
    ) -> PermissionStatus {
        match permission {
            Permission::Microphone => request_mic(),
            Permission::ScreenRecording | Permission::Accessibility => {
                self.check_permission(permission)
            },
        }
    }

    fn enumerate_apps(&self) -> Vec<AppInfo> {
        let workspace = NSWorkspace::sharedWorkspace();
        let running = workspace.runningApplications();
        let mut out = Vec::new();

        for app in &running {
            if app.activationPolicy() != NSApplicationActivationPolicy::Regular {
                continue;
            }
            let pid = app.processIdentifier();
            if pid <= 0 {
                continue;
            }
            out.push(AppInfo {
                pid,
                name: ns_to_string(app.localizedName().as_deref()).unwrap_or_default(),
                bundle_id: ns_to_string(app.bundleIdentifier().as_deref()),
                has_audio: has_audio_object(pid),
            });
        }
        out
    }
}

/// Registers the macOS discovery provider if none is present.
///
/// Idempotent: leaves an existing (e.g. Swift / test) provider untouched.
#[must_use]
pub fn install_default_native_provider() -> bool {
    if crate::native::native_provider_registered() {
        return true;
    }
    register_native_provider(Box::new(MacosDiscoveryProvider));
    crate::native::native_provider_registered()
}

//! macOS system queries used by `koe info` (and locale listing).
//!
//! Device and speech-locale lookups live here rather than on
//! [`crate::NativeProvider`]: the CLI already installs an in-process discovery
//! provider, and these queries do not need the Swift bridge.
//!
//! Unlike permission/app enumeration, these helpers are process-global OS
//! reads and are not injectable via [`crate::NativeProvider`] (intentional: keeps the
//! `UniFFI` callback surface small). On non-macOS hosts they degrade to
//! `None` / empty via stubs in `lib.rs`.

#![allow(unsafe_code)]

use std::ffi::c_void;
use std::ptr;

use objc2_speech::SFSpeechRecognizer;

use crate::types::AudioDeviceInfo;

type AudioObjectID = u32;
type OSStatus = i32;
type CFStringRef = *const c_void;
type CFIndex = isize;
type CFStringEncoding = u32;

const K_CF_STRING_ENCODING_UTF8: CFStringEncoding = 0x0800_0100;

#[repr(C)]
struct AudioObjectPropertyAddress {
    selector: u32,
    scope: u32,
    element: u32,
}

const AUDIO_OBJECT_SYSTEM_OBJECT: AudioObjectID = 1;
const AUDIO_OBJECT_UNKNOWN: AudioObjectID = 0;
const AUDIO_OBJECT_PROPERTY_ELEMENT_MAIN: u32 = 0;
const AUDIO_OBJECT_PROPERTY_SCOPE_GLOBAL: u32 = u32::from_be_bytes(*b"glob");
const AUDIO_HARDWARE_PROPERTY_DEFAULT_INPUT_DEVICE: u32 = u32::from_be_bytes(*b"dIn ");
const AUDIO_HARDWARE_PROPERTY_DEFAULT_OUTPUT_DEVICE: u32 = u32::from_be_bytes(*b"dOut");
const AUDIO_OBJECT_PROPERTY_NAME: u32 = u32::from_be_bytes(*b"lnam");
const AUDIO_DEVICE_PROPERTY_DEVICE_UID: u32 = u32::from_be_bytes(*b"uid ");

#[link(name = "CoreAudio", kind = "framework")]
unsafe extern "C" {
    fn AudioObjectGetPropertyData(
        object_id: AudioObjectID,
        address: *const AudioObjectPropertyAddress,
        qualifier_data_size: u32,
        qualifier_data: *const c_void,
        data_size: *mut u32,
        data: *mut c_void,
    ) -> OSStatus;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(cf: CFStringRef);
    fn CFStringGetLength(the_string: CFStringRef) -> CFIndex;
    fn CFStringGetMaximumSizeForEncoding(
        length: CFIndex,
        encoding: CFStringEncoding,
    ) -> CFIndex;
    fn CFStringGetCString(
        the_string: CFStringRef,
        buffer: *mut u8,
        buffer_size: CFIndex,
        encoding: CFStringEncoding,
    ) -> u8;
}

/// Default input device name + UID, when Core Audio reports one.
#[must_use]
pub fn default_input_device() -> Option<AudioDeviceInfo> {
    default_device(AUDIO_HARDWARE_PROPERTY_DEFAULT_INPUT_DEVICE)
}

/// Default output device name + UID, when Core Audio reports one.
#[must_use]
pub fn default_output_device() -> Option<AudioDeviceInfo> {
    default_device(AUDIO_HARDWARE_PROPERTY_DEFAULT_OUTPUT_DEVICE)
}

/// BCP-47 locale identifiers from `SFSpeechRecognizer.supportedLocales()`.
///
/// Identifiers are normalized from Apple's underscore form (`en_US`) to
/// BCP-47 hyphen form (`en-US`) so they match `--locale` / pipeline
/// conventions. Sorted for stable CLI / JSON output. Empty if the Speech
/// framework returns nothing (unusual on a stock macOS install).
#[must_use]
pub fn supported_speech_locales() -> Vec<String> {
    // SAFETY: class method; returns an immutable set of NSLocale.
    let locales = unsafe { SFSpeechRecognizer::supportedLocales() };
    let mut out: Vec<String> = locales
        .iter()
        .map(|locale| to_bcp47(&locale.localeIdentifier().to_string()))
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// Apple `localeIdentifier` uses `_`; Koe CLI/pipeline locale flags use `-`.
fn to_bcp47(identifier: &str) -> String {
    identifier.replace('_', "-")
}

fn default_device(selector: u32) -> Option<AudioDeviceInfo> {
    let device_id = audio_object_id(AUDIO_OBJECT_SYSTEM_OBJECT, selector)?;
    if device_id == AUDIO_OBJECT_UNKNOWN {
        return None;
    }
    let name = cf_string_property(device_id, AUDIO_OBJECT_PROPERTY_NAME)?;
    let uid = cf_string_property(device_id, AUDIO_DEVICE_PROPERTY_DEVICE_UID)?;
    Some(AudioDeviceInfo { name, uid })
}

fn audio_object_id(
    object_id: AudioObjectID,
    selector: u32,
) -> Option<AudioObjectID> {
    let address = AudioObjectPropertyAddress {
        selector,
        scope: AUDIO_OBJECT_PROPERTY_SCOPE_GLOBAL,
        element: AUDIO_OBJECT_PROPERTY_ELEMENT_MAIN,
    };
    let mut value = AUDIO_OBJECT_UNKNOWN;
    let mut size = u32::try_from(size_of::<AudioObjectID>()).unwrap_or(4);
    // SAFETY: Core Audio C ABI; out buffer matches `size`.
    let status = unsafe {
        AudioObjectGetPropertyData(
            object_id,
            &raw const address,
            0,
            ptr::null(),
            &raw mut size,
            (&raw mut value).cast(),
        )
    };
    (status == 0).then_some(value)
}

fn cf_string_property(
    object_id: AudioObjectID,
    selector: u32,
) -> Option<String> {
    let address = AudioObjectPropertyAddress {
        selector,
        scope: AUDIO_OBJECT_PROPERTY_SCOPE_GLOBAL,
        element: AUDIO_OBJECT_PROPERTY_ELEMENT_MAIN,
    };
    let mut cf_string: CFStringRef = ptr::null();
    let mut size = u32::try_from(size_of::<CFStringRef>()).unwrap_or(8);
    // SAFETY: Core Audio writes a CFStringRef we must release.
    let status = unsafe {
        AudioObjectGetPropertyData(
            object_id,
            &raw const address,
            0,
            ptr::null(),
            &raw mut size,
            (&raw mut cf_string).cast(),
        )
    };
    if status != 0 || cf_string.is_null() {
        return None;
    }
    // Release even if conversion panics or returns None.
    let _owned = CfString(cf_string);
    cf_string_to_string(cf_string)
}

/// Owns a `CFString` and releases it on drop.
struct CfString(CFStringRef);

impl Drop for CfString {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: we uniquely own this CFString from AudioObjectGetPropertyData.
            unsafe { CFRelease(self.0) };
        }
    }
}

fn cf_string_to_string(cf_string: CFStringRef) -> Option<String> {
    // SAFETY: `cf_string` is a valid CFString for the duration of this call.
    let length = unsafe { CFStringGetLength(cf_string) };
    if length < 0 {
        return None;
    }
    let max_size =
        // SAFETY: documented CFString sizing helper.
        unsafe { CFStringGetMaximumSizeForEncoding(length, K_CF_STRING_ENCODING_UTF8) };
    if max_size < 0 {
        return None;
    }
    // +1 for the NUL that CFStringGetCString writes.
    let buf_len = usize::try_from(max_size).ok()?.checked_add(1)?;
    let buffer_size = CFIndex::try_from(buf_len).ok()?;
    let mut buf = vec![0u8; buf_len];
    // SAFETY: buffer is writable and sized per CFStringGetMaximumSizeForEncoding.
    let ok = unsafe {
        CFStringGetCString(
            cf_string,
            buf.as_mut_ptr(),
            buffer_size,
            K_CF_STRING_ENCODING_UTF8,
        )
    };
    if ok == 0 {
        return None;
    }
    let nul = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    std::str::from_utf8(&buf[..nul]).ok().map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::to_bcp47;

    #[test]
    fn to_bcp47_normalizes_underscore() {
        assert_eq!(to_bcp47("en_US"), "en-US");
        assert_eq!(to_bcp47("zh_Hans_CN"), "zh-Hans-CN");
        assert_eq!(to_bcp47("ja-JP"), "ja-JP");
    }
}

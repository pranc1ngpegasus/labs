//! Core Audio Process Tap capture for a single PID (macOS 14.2+).

#![allow(
    clippy::cast_possible_wrap,
    clippy::expect_used,
    clippy::needless_pass_by_value,
    clippy::unwrap_used
)]

use std::ffi::c_void;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};

use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject};
use objc2_foundation::{NSArray, NSNumber, NSString};

use crate::error::CaptureError;
use crate::handles::CaptureHandle;

use super::{CaptureSession, monotonic_ms};

type AudioObjectID = u32;
type OSStatus = i32;
type AudioDeviceIOProcID = *mut c_void;

#[repr(C)]
struct AudioObjectPropertyAddress {
    selector: u32,
    scope: u32,
    element: u32,
}

#[repr(C)]
struct AudioBuffer {
    m_number_channels: u32,
    m_data_byte_size: u32,
    m_data: *mut c_void,
}

#[repr(C)]
struct AudioBufferList {
    m_number_buffers: u32,
    m_buffers: [AudioBuffer; 1],
}

#[repr(C)]
struct AudioTimeStamp {
    _data: [u8; 64],
}

const AUDIO_OBJECT_SYSTEM_OBJECT: AudioObjectID = 1;
const AUDIO_OBJECT_UNKNOWN: AudioObjectID = 0;
const AUDIO_OBJECT_PROPERTY_SCOPE_GLOBAL: u32 = u32::from_be_bytes(*b"glob");
const AUDIO_OBJECT_PROPERTY_ELEMENT_MAIN: u32 = 0;
const AUDIO_HARDWARE_TRANSLATE_PID_TO_PROCESS_OBJECT: u32 = u32::from_be_bytes(*b"ptid");
const STREAM_FORMAT_SELECTOR: u32 = u32::from_be_bytes(*b"sfmt");

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
    fn AudioHardwareCreateProcessTap(
        description: *mut AnyObject,
        out_tap_id: *mut AudioObjectID,
    ) -> OSStatus;
    fn AudioHardwareDestroyProcessTap(tap_id: AudioObjectID) -> OSStatus;
    fn AudioDeviceCreateIOProcID(
        device: AudioObjectID,
        io_proc: unsafe extern "C-unwind" fn(
            AudioObjectID,
            *const AudioTimeStamp,
            *const AudioBufferList,
            *const AudioTimeStamp,
            *mut AudioBufferList,
            *const AudioTimeStamp,
            *mut c_void,
        ) -> OSStatus,
        client_data: *mut c_void,
        out_proc_id: *mut AudioDeviceIOProcID,
    ) -> OSStatus;
    fn AudioDeviceDestroyIOProcID(
        device: AudioObjectID,
        io_proc_id: AudioDeviceIOProcID,
    ) -> OSStatus;
    fn AudioDeviceStart(
        device: AudioObjectID,
        io_proc_id: AudioDeviceIOProcID,
    ) -> OSStatus;
    fn AudioDeviceStop(
        device: AudioObjectID,
        io_proc_id: AudioDeviceIOProcID,
    ) -> OSStatus;
}

struct TapState {
    /// Weak to avoid CaptureHandle ↔ session ↔ TapState cycles.
    handle: Weak<CaptureHandle>,
    tap_id: AudioObjectID,
    io_proc_id: AudioDeviceIOProcID,
    running: AtomicBool,
    channels: u32,
}

pub(super) struct ProcessTapSession {
    state: *mut TapState,
}

// SAFETY: real-time IO proc only reads immutable setup + atomics / Arc handle.
unsafe impl Send for ProcessTapSession {}

pub(super) fn start(
    pid: i32,
    handle: Arc<CaptureHandle>,
) -> Result<Box<dyn CaptureSession>, CaptureError> {
    if pid <= 0 {
        return Err(CaptureError::Internal {
            msg: format!("invalid pid: {pid}"),
        });
    }

    let process_id = process_object_for_pid(pid).ok_or_else(|| CaptureError::StreamError {
        msg: format!("no Core Audio process object for pid {pid}"),
    })?;

    start_with_description(make_tap_description(&[process_id])?, handle)
}

/// Stereo mixdown of every process except this one (AppAudio fallback).
pub(super) fn start_global(
    handle: Arc<CaptureHandle>
) -> Result<Box<dyn CaptureSession>, CaptureError> {
    let exclude = process_object_for_pid(std::process::id() as i32)
        .into_iter()
        .collect::<Vec<_>>();
    start_with_description(make_global_tap_description(&exclude)?, handle)
}

fn start_with_description(
    description: Retained<AnyObject>,
    handle: Arc<CaptureHandle>,
) -> Result<Box<dyn CaptureSession>, CaptureError> {
    let mut tap_id = AUDIO_OBJECT_UNKNOWN;
    // SAFETY: CATapDescription retained for the call duration.
    let status = unsafe {
        AudioHardwareCreateProcessTap(Retained::as_ptr(&description).cast_mut(), &raw mut tap_id)
    };
    if status != 0 || tap_id == AUDIO_OBJECT_UNKNOWN {
        return Err(CaptureError::StreamError {
            msg: format!("AudioHardwareCreateProcessTap failed: {status}"),
        });
    }

    let channels = stream_channel_count(tap_id).unwrap_or(2).max(1);

    let state = Box::into_raw(Box::new(TapState {
        handle: Arc::downgrade(&handle),
        tap_id,
        io_proc_id: ptr::null_mut(),
        running: AtomicBool::new(false),
        channels,
    }));
    drop(handle);

    let mut io_proc: AudioDeviceIOProcID = ptr::null_mut();
    let create =
        unsafe { AudioDeviceCreateIOProcID(tap_id, tap_io_proc, state.cast(), &raw mut io_proc) };
    if create != 0 || io_proc.is_null() {
        unsafe {
            let _ = AudioHardwareDestroyProcessTap(tap_id);
            drop(Box::from_raw(state));
        }
        return Err(CaptureError::StreamError {
            msg: format!("AudioDeviceCreateIOProcID failed: {create}"),
        });
    }

    unsafe {
        (*state).io_proc_id = io_proc;
    }

    let start = unsafe { AudioDeviceStart(tap_id, io_proc) };
    if start != 0 {
        stop_raw(state);
        return Err(CaptureError::StreamError {
            msg: format!("AudioDeviceStart failed: {start}"),
        });
    }

    unsafe {
        (*state).running.store(true, Ordering::Release);
    }

    drop(description);
    Ok(Box::new(ProcessTapSession { state }))
}

fn make_tap_description(
    process_object_ids: &[AudioObjectID]
) -> Result<Retained<AnyObject>, CaptureError> {
    let class = AnyClass::get(c"CATapDescription").ok_or_else(|| CaptureError::Internal {
        msg: "CATapDescription class missing (need macOS 14.2+)".to_owned(),
    })?;

    let numbers: Vec<Retained<NSNumber>> = process_object_ids
        .iter()
        .map(|id| NSNumber::new_u32(*id))
        .collect();
    let refs: Vec<&NSNumber> = numbers.iter().map(std::ops::Deref::deref).collect();
    let processes: Retained<NSArray<NSNumber>> = NSArray::from_slice(&refs);

    // SAFETY: documented Objective-C initializer on CoreAudio's CATapDescription.
    let alloc: *mut AnyObject = unsafe { msg_send![class, alloc] };
    let desc: *mut AnyObject =
        unsafe { msg_send![alloc, initStereoMixdownOfProcesses: &*processes] };
    if desc.is_null() {
        return Err(CaptureError::Internal {
            msg: "CATapDescription init failed".to_owned(),
        });
    }
    let desc = unsafe { Retained::from_raw(desc).expect("non-null desc") };

    let name = NSString::from_str("Koe Process Tap");
    let _: () = unsafe { msg_send![&*desc, setName: &*name] };
    Ok(desc)
}

fn make_global_tap_description(
    exclude: &[AudioObjectID]
) -> Result<Retained<AnyObject>, CaptureError> {
    let class = AnyClass::get(c"CATapDescription").ok_or_else(|| CaptureError::Internal {
        msg: "CATapDescription class missing (need macOS 14.2+)".to_owned(),
    })?;

    let numbers: Vec<Retained<NSNumber>> =
        exclude.iter().map(|id| NSNumber::new_u32(*id)).collect();
    let refs: Vec<&NSNumber> = numbers.iter().map(std::ops::Deref::deref).collect();
    let processes: Retained<NSArray<NSNumber>> = NSArray::from_slice(&refs);

    let alloc: *mut AnyObject = unsafe { msg_send![class, alloc] };
    let desc: *mut AnyObject =
        unsafe { msg_send![alloc, initStereoGlobalTapButExcludeProcesses: &*processes] };
    if desc.is_null() {
        return Err(CaptureError::Internal {
            msg: "CATapDescription global init failed".to_owned(),
        });
    }
    let desc = unsafe { Retained::from_raw(desc).expect("non-null desc") };
    let name = NSString::from_str("Koe Global Process Tap");
    let _: () = unsafe { msg_send![&*desc, setName: &*name] };
    Ok(desc)
}

fn process_object_for_pid(pid: i32) -> Option<AudioObjectID> {
    let address = AudioObjectPropertyAddress {
        selector: AUDIO_HARDWARE_TRANSLATE_PID_TO_PROCESS_OBJECT,
        scope: AUDIO_OBJECT_PROPERTY_SCOPE_GLOBAL,
        element: AUDIO_OBJECT_PROPERTY_ELEMENT_MAIN,
    };
    let mut process_object = AUDIO_OBJECT_UNKNOWN;
    let mut size = u32::try_from(size_of::<AudioObjectID>()).unwrap_or(4);
    let mutable_pid = pid;
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
    (status == 0 && process_object != AUDIO_OBJECT_UNKNOWN).then_some(process_object)
}

fn stream_channel_count(tap_id: AudioObjectID) -> Option<u32> {
    #[repr(C)]
    struct AudioStreamBasicDescription {
        sample_rate: f64,
        format_id: u32,
        format_flags: u32,
        bytes_per_packet: u32,
        frames_per_packet: u32,
        bytes_per_frame: u32,
        channels_per_frame: u32,
        bits_per_channel: u32,
        reserved: u32,
    }

    let address = AudioObjectPropertyAddress {
        selector: STREAM_FORMAT_SELECTOR,
        scope: AUDIO_OBJECT_PROPERTY_SCOPE_GLOBAL,
        element: AUDIO_OBJECT_PROPERTY_ELEMENT_MAIN,
    };
    let mut format = AudioStreamBasicDescription {
        sample_rate: 0.0,
        format_id: 0,
        format_flags: 0,
        bytes_per_packet: 0,
        frames_per_packet: 0,
        bytes_per_frame: 0,
        channels_per_frame: 0,
        bits_per_channel: 0,
        reserved: 0,
    };
    let mut size = u32::try_from(size_of::<AudioStreamBasicDescription>()).unwrap_or(40);
    let status = unsafe {
        AudioObjectGetPropertyData(
            tap_id,
            &raw const address,
            0,
            ptr::null(),
            &raw mut size,
            (&raw mut format).cast(),
        )
    };
    (status == 0 && format.channels_per_frame > 0).then_some(format.channels_per_frame)
}

fn stop_raw(state: *mut TapState) {
    let state_ref = unsafe { &*state };
    state_ref.running.store(false, Ordering::Release);
    let tap_id = state_ref.tap_id;
    let io_proc = state_ref.io_proc_id;
    if !io_proc.is_null() {
        unsafe {
            let _ = AudioDeviceStop(tap_id, io_proc);
            let _ = AudioDeviceDestroyIOProcID(tap_id, io_proc);
        }
    }
    if tap_id != AUDIO_OBJECT_UNKNOWN {
        unsafe {
            let _ = AudioHardwareDestroyProcessTap(tap_id);
        }
    }
    drop(unsafe { Box::from_raw(state) });
}

impl CaptureSession for ProcessTapSession {
    fn stop(&mut self) {
        if self.state.is_null() {
            return;
        }
        let state = std::mem::replace(&mut self.state, ptr::null_mut());
        stop_raw(state);
    }
}

impl Drop for ProcessTapSession {
    fn drop(&mut self) {
        self.stop();
    }
}

unsafe extern "C-unwind" fn tap_io_proc(
    _device: AudioObjectID,
    _now: *const AudioTimeStamp,
    input_data: *const AudioBufferList,
    _input_time: *const AudioTimeStamp,
    output_data: *mut AudioBufferList,
    _output_time: *const AudioTimeStamp,
    client_data: *mut c_void,
) -> OSStatus {
    if client_data.is_null() {
        return 0;
    }
    // SAFETY: `client_data` is TapState from `start`, alive until stop.
    let state = unsafe { &*(client_data.cast::<TapState>()) };

    // Passthrough so the tapped process still reaches hardware.
    if !input_data.is_null() && !output_data.is_null() {
        copy_buffer_list(input_data, output_data);
    }

    if !state.running.load(Ordering::Acquire) || input_data.is_null() {
        return 0;
    }

    // SAFETY: CoreAudio provides a valid AudioBufferList for the IO callback.
    let list = unsafe { &*input_data };
    if list.m_number_buffers == 0 {
        return 0;
    }
    let buffer = &list.m_buffers[0];
    if buffer.m_data.is_null() || buffer.m_data_byte_size < 4 {
        return 0;
    }

    let sample_count = (buffer.m_data_byte_size as usize) / 4;
    // SAFETY: buffer size is reported by CoreAudio in bytes.
    let src = unsafe { std::slice::from_raw_parts(buffer.m_data.cast::<f32>(), sample_count) };
    let pcm = to_stereo_interleaved(src, state.channels as usize);
    if !pcm.is_empty()
        && let Some(handle) = state.handle.upgrade()
    {
        handle.deliver_audio(pcm, monotonic_ms());
    }
    0
}

fn copy_buffer_list(
    input: *const AudioBufferList,
    output: *mut AudioBufferList,
) {
    let input = unsafe { &*input };
    let output = unsafe { &mut *output };
    let count = input.m_number_buffers.min(output.m_number_buffers) as usize;
    for i in 0..count {
        let src = unsafe { &*input.m_buffers.as_ptr().add(i) };
        let dst = unsafe { &mut *output.m_buffers.as_mut_ptr().add(i) };
        if src.m_data.is_null() || dst.m_data.is_null() || src.m_data_byte_size == 0 {
            continue;
        }
        let n = src.m_data_byte_size.min(dst.m_data_byte_size) as usize;
        unsafe {
            ptr::copy_nonoverlapping(src.m_data.cast::<u8>(), dst.m_data.cast::<u8>(), n);
        }
        dst.m_data_byte_size = u32::try_from(n).unwrap_or(0);
    }
}

fn to_stereo_interleaved(
    src: &[f32],
    channels: usize,
) -> Vec<f32> {
    if channels == 0 || src.is_empty() {
        return Vec::new();
    }
    if channels == 2 {
        return src.to_vec();
    }
    if channels == 1 {
        let mut out = Vec::with_capacity(src.len() * 2);
        for &s in src {
            out.push(s);
            out.push(s);
        }
        return out;
    }
    // Downmix first two channels when more are present.
    let frames = src.len() / channels;
    let mut out = Vec::with_capacity(frames * 2);
    for frame in 0..frames {
        let base = frame * channels;
        out.push(src[base]);
        out.push(src.get(base + 1).copied().unwrap_or(src[base]));
    }
    out
}

//! Microphone capture via AudioQueue (48 kHz stereo Float32 interleaved).

#![allow(clippy::needless_pass_by_value)]

use std::ffi::c_void;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, Ordering};
use std::sync::{Arc, Weak};

use crate::error::CaptureError;
use crate::handles::CaptureHandle;

use super::{CaptureSession, monotonic_ms};

type OSStatus = i32;
type AudioQueueRef = *mut c_void;
type AudioQueueBufferRef = *mut AudioQueueBuffer;

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

#[repr(C)]
struct AudioQueueBuffer {
    audio_data_bytes_capacity: u32,
    audio_data: *mut c_void,
    audio_data_byte_size: u32,
    user_data: *mut c_void,
    packet_description_capacity: u32,
    packet_descriptions: *mut c_void,
    packet_description_count: u32,
}

const K_AUDIO_FORMAT_LINEAR_PCM: u32 = u32::from_be_bytes(*b"lpcm");
const K_AUDIO_FORMAT_FLAG_IS_FLOAT: u32 = 1;
const K_AUDIO_FORMAT_FLAG_IS_PACKED: u32 = 8;
/// Number of 20 ms host buffers kept in flight (frames = 960 at 48 kHz).
const BUFFER_FRAMES: u32 = 960;
const BUFFER_COUNT: usize = 3;

/// Soft AGC target / limits. Built-in mics often sit around 0.01–0.05 peak
/// without Apple's Voice-Processing unit; we lift toward conversational level.
const AGC_TARGET_PEAK: f32 = 0.45;
const AGC_MAX_GAIN: f32 = 30.0;
const AGC_MIN_GAIN: f32 = 1.0;

#[link(name = "AudioToolbox", kind = "framework")]
unsafe extern "C" {
    fn AudioQueueNewInput(
        in_format: *const AudioStreamBasicDescription,
        in_callback: unsafe extern "C-unwind" fn(
            *mut c_void,
            AudioQueueRef,
            AudioQueueBufferRef,
            *const c_void,
            u32,
            *const c_void,
        ),
        in_user_data: *mut c_void,
        in_callback_run_loop: *mut c_void,
        in_callback_run_loop_mode: *const c_void,
        in_flags: u32,
        out_aq: *mut AudioQueueRef,
    ) -> OSStatus;
    fn AudioQueueAllocateBuffer(
        in_aq: AudioQueueRef,
        in_buffer_byte_size: u32,
        out_buffer: *mut AudioQueueBufferRef,
    ) -> OSStatus;
    fn AudioQueueEnqueueBuffer(
        in_aq: AudioQueueRef,
        in_buffer: AudioQueueBufferRef,
        in_num_packet_descs: u32,
        in_packet_descs: *const c_void,
    ) -> OSStatus;
    fn AudioQueueStart(
        in_aq: AudioQueueRef,
        in_start_time: *const c_void,
    ) -> OSStatus;
    fn AudioQueueStop(
        in_aq: AudioQueueRef,
        in_immediate: u8,
    ) -> OSStatus;
    fn AudioQueueDispose(
        in_aq: AudioQueueRef,
        in_immediate: u8,
    ) -> OSStatus;
}

struct MicState {
    /// Weak to avoid `CaptureHandle` ↔ session ↔ `MicState` Arc cycles.
    handle: Weak<CaptureHandle>,
    /// Written once after queue creation; callbacks only read after start.
    queue: AtomicPtr<c_void>,
    running: AtomicBool,
    /// Smoothed abs-peak envelope (`f32` bits) for soft AGC.
    envelope_bits: AtomicU32,
}

pub(super) struct MicrophoneSession {
    state: Option<Arc<MicState>>,
    /// Balances the `Arc::into_raw` passed to AudioQueue as user data.
    callback_arc: *const MicState,
}

// SAFETY: AudioQueue callbacks run on a dedicated queue thread; we only touch
// atomics and forward owned PCM copies through the CaptureHandle Arc.
unsafe impl Send for MicrophoneSession {}

pub(super) fn start(handle: Arc<CaptureHandle>) -> Result<Box<dyn CaptureSession>, CaptureError> {
    let format = AudioStreamBasicDescription {
        sample_rate: 48_000.0,
        format_id: K_AUDIO_FORMAT_LINEAR_PCM,
        format_flags: K_AUDIO_FORMAT_FLAG_IS_FLOAT | K_AUDIO_FORMAT_FLAG_IS_PACKED,
        bytes_per_packet: 8,
        frames_per_packet: 1,
        bytes_per_frame: 8,
        channels_per_frame: 2,
        bits_per_channel: 32,
        reserved: 0,
    };

    let state = Arc::new(MicState {
        handle: Arc::downgrade(&handle),
        queue: AtomicPtr::new(ptr::null_mut()),
        running: AtomicBool::new(false),
        envelope_bits: AtomicU32::new((1e-3_f32).to_bits()),
    });
    drop(handle);
    let callback_arc = Arc::into_raw(Arc::clone(&state));

    let mut queue: AudioQueueRef = ptr::null_mut();
    // SAFETY: AudioQueue C ABI; user data is an Arc kept alive until stop.
    let status = unsafe {
        AudioQueueNewInput(
            &raw const format,
            mic_input_callback,
            callback_arc.cast_mut().cast(),
            ptr::null_mut(),
            ptr::null(),
            0,
            &raw mut queue,
        )
    };
    if status != 0 || queue.is_null() {
        // SAFETY: paired with into_raw above; queue was not created.
        drop(unsafe { Arc::from_raw(callback_arc) });
        return Err(CaptureError::StreamError {
            msg: format!("AudioQueueNewInput failed: {status}"),
        });
    }

    state.queue.store(queue, Ordering::Release);

    let byte_size = BUFFER_FRAMES * 8;
    for _ in 0..BUFFER_COUNT {
        let mut buffer: AudioQueueBufferRef = ptr::null_mut();
        let alloc = unsafe { AudioQueueAllocateBuffer(queue, byte_size, &raw mut buffer) };
        if alloc != 0 || buffer.is_null() {
            tear_down(Some(state), callback_arc);
            return Err(CaptureError::StreamError {
                msg: format!("AudioQueueAllocateBuffer failed: {alloc}"),
            });
        }
        let enq = unsafe { AudioQueueEnqueueBuffer(queue, buffer, 0, ptr::null()) };
        if enq != 0 {
            tear_down(Some(state), callback_arc);
            return Err(CaptureError::StreamError {
                msg: format!("AudioQueueEnqueueBuffer failed: {enq}"),
            });
        }
    }

    let start = unsafe { AudioQueueStart(queue, ptr::null()) };
    if start != 0 {
        tear_down(Some(state), callback_arc);
        return Err(CaptureError::StreamError {
            msg: format!("AudioQueueStart failed: {start}"),
        });
    }

    state.running.store(true, Ordering::Release);

    Ok(Box::new(MicrophoneSession {
        state: Some(state),
        callback_arc,
    }))
}

fn tear_down(
    state: Option<Arc<MicState>>,
    callback_arc: *const MicState,
) {
    if let Some(state) = state {
        state.running.store(false, Ordering::Release);
        let queue = state.queue.swap(ptr::null_mut(), Ordering::AcqRel);
        if !queue.is_null() {
            unsafe {
                // Non-immediate stop waits for in-flight callbacks before return.
                let _ = AudioQueueStop(queue, 0);
                let _ = AudioQueueDispose(queue, 0);
            }
        }
    }
    if !callback_arc.is_null() {
        // SAFETY: balances Arc::into_raw after AudioQueueStop drained callbacks.
        drop(unsafe { Arc::from_raw(callback_arc) });
    }
}

impl CaptureSession for MicrophoneSession {
    fn stop(&mut self) {
        let state = self.state.take();
        let callback_arc = std::mem::replace(&mut self.callback_arc, ptr::null());
        tear_down(state, callback_arc);
    }
}

impl Drop for MicrophoneSession {
    fn drop(&mut self) {
        self.stop();
    }
}

unsafe extern "C-unwind" fn mic_input_callback(
    user_data: *mut c_void,
    in_aq: AudioQueueRef,
    in_buffer: AudioQueueBufferRef,
    _start_time: *const c_void,
    _num_packets: u32,
    _packet_descs: *const c_void,
) {
    if user_data.is_null() || in_buffer.is_null() {
        return;
    }
    // SAFETY: user_data is Arc::into_raw(MicState); alive until tear_down's from_raw.
    let state = unsafe { &*user_data.cast::<MicState>() };
    if !state.running.load(Ordering::Acquire) {
        return;
    }

    // SAFETY: AudioQueue owns the buffer for the duration of this callback.
    let buffer = unsafe { &*in_buffer };
    let byte_count = buffer.audio_data_byte_size as usize;
    if byte_count >= 4 && !buffer.audio_data.is_null() {
        let sample_count = byte_count / 4;
        // SAFETY: AudioQueue guarantees `audio_data` holds `byte_count` bytes of PCM.
        let samples =
            unsafe { std::slice::from_raw_parts(buffer.audio_data.cast::<f32>(), sample_count) };
        if let Some(handle) = state.handle.upgrade() {
            handle.deliver_audio(apply_agc(state, samples), monotonic_ms());
        }
    }

    if state.running.load(Ordering::Acquire) {
        let _ = unsafe { AudioQueueEnqueueBuffer(in_aq, in_buffer, 0, ptr::null()) };
    }
}

fn apply_agc(
    state: &MicState,
    samples: &[f32],
) -> Vec<f32> {
    let mut block_peak = 1e-6_f32;
    for &s in samples {
        block_peak = block_peak.max(s.abs());
    }

    let prev = f32::from_bits(state.envelope_bits.load(Ordering::Relaxed));
    // Fast attack, slow release so speech onsets aren't clipped and quiet
    // passages still get makeup.
    let envelope = if block_peak > prev {
        block_peak
    } else {
        0.02f32.mul_add(block_peak, 0.98 * prev)
    };
    state
        .envelope_bits
        .store(envelope.to_bits(), Ordering::Relaxed);

    let gain = (AGC_TARGET_PEAK / envelope.max(1e-4)).clamp(AGC_MIN_GAIN, AGC_MAX_GAIN);
    samples
        .iter()
        .map(|s| (s * gain).clamp(-1.0, 1.0))
        .collect()
}

//! macOS system-audio capture via `ScreenCaptureKit` (driver-free loopback).
//!
//! Opens a [`SCStream`] on a whole-display [`SCContentFilter`] with
//! `capturesAudio`, delivering interleaved Float32 (48 kHz, stereo) PCM through
//! the [`SCStreamOutput`] protocol on a serial dispatch queue. Each audio
//! callback is repackaged as an owned [`AudioFrameOwned`] and forwarded to the
//! caller's bounded channel with drop-oldest backpressure.
//!
//! `ScreenCaptureKit` completion handlers fire on the main runloop, so start/stop
//! pump that runloop while awaiting results (the same approach `koe` uses).

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_ptr_alignment,
    clippy::cast_sign_loss,
    clippy::needless_pass_by_value,
    clippy::non_send_fields_in_send_ty,
    clippy::struct_field_names,
    clippy::unwrap_used,
    unsafe_code
)]

use std::alloc::{Layout, alloc, dealloc};
use std::ffi::c_void;
use std::ptr;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

use block2::RcBlock;
use dispatch2::{DispatchQueue, DispatchQueueAttr, DispatchRetained};
use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{AnyThread, DefinedClass, MainThreadMarker, define_class, msg_send};
use objc2_app_kit::NSApplication;
use objc2_core_media::{CMSampleBuffer, CMTime};
use objc2_foundation::{NSArray, NSError};
use objc2_screen_capture_kit::{
    SCContentFilter, SCShareableContent, SCStream, SCStreamConfiguration, SCStreamOutput,
    SCStreamOutputType,
};

use crate::{AudioFormat, AudioFrameOwned, Error};

/// The canonical capture format: stereo Float32 at 48 kHz.
const SAMPLE_RATE: i32 = 48_000;
const CHANNELS: i32 = 2;
/// 'BGRA' — minimal video plane; only the audio output is actually consumed.
const PIXEL_FORMAT_BGRA: u32 = u32::from_be_bytes(*b"BGRA");
const ASSURE_16_BYTE_ALIGNMENT: u32 = 1 << 0;
const SCK_WAIT_TIMEOUT: Duration = Duration::from_secs(15);
const SCK_STOP_TIMEOUT: Duration = Duration::from_secs(5);

type AudioSink = Arc<dyn Fn(Vec<f32>) + Send + Sync>;
type CFStringRef = *const c_void;
type CFRunLoopMode = CFStringRef;
type CFTimeInterval = f64;
type Boolean = u8;

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRunLoopRunInMode(
        mode: CFRunLoopMode,
        seconds: CFTimeInterval,
        return_after_source_handled: Boolean,
    ) -> i32;
    static kCFRunLoopDefaultMode: CFStringRef;
    fn CFRelease(cf: *const c_void);
}

/// A running macOS system-capture session.
pub(super) struct MacSystemCapture {
    stream: Retained<SCStream>,
    output: Retained<AudioOutput>,
    _queue: DispatchRetained<DispatchQueue>,
    dropped: Arc<AtomicUsize>,
    stopped: Mutex<bool>,
}

// SAFETY: stop is synchronized; SCStream callbacks run on our serial queue.
unsafe impl Send for MacSystemCapture {}

impl MacSystemCapture {
    pub(super) fn start(
        sender: std::sync::mpsc::SyncSender<AudioFrameOwned>,
        dropped: Arc<AtomicUsize>,
    ) -> Result<Self, Error> {
        // ScreenCaptureKit / CoreGraphics require AppKit initialization.
        let Some(mtm) = MainThreadMarker::new() else {
            return Err(Error::Device(
                "ScreenCaptureKit must start on the main thread".to_owned(),
            ));
        };
        let app = NSApplication::sharedApplication(mtm);
        let _ = app.setActivationPolicy(objc2_app_kit::NSApplicationActivationPolicy::Accessory);

        let content = shareable_content()?;
        let filter = make_system_filter(&content)?;
        let config = make_audio_config();

        let sink_dropped = Arc::clone(&dropped);
        let sink: AudioSink = Arc::new(move |pcm: Vec<f32>| {
            let frames = (pcm.len() / CHANNELS as usize) as i32;
            let owned = AudioFrameOwned {
                data: bytes_of_f32(&pcm),
                frames,
                channels: CHANNELS,
                sample_rate: SAMPLE_RATE,
                format: AudioFormat::F32,
                timestamp_us: 0,
            };
            if sender.try_send(owned).is_err() {
                sink_dropped.fetch_add(1, Ordering::Relaxed);
            }
        });
        let output = AudioOutput::new(sink);
        let queue = DispatchQueue::new("dev.mokmok.oto.sck.system", DispatchQueueAttr::SERIAL);

        let stream = unsafe {
            SCStream::initWithFilter_configuration_delegate(
                SCStream::alloc(),
                &filter,
                &config,
                None,
            )
        };

        let proto: &ProtocolObject<dyn SCStreamOutput> = ProtocolObject::from_ref(&*output);
        unsafe {
            stream
                .addStreamOutput_type_sampleHandlerQueue_error(
                    proto,
                    SCStreamOutputType::Audio,
                    Some(&queue),
                )
                .map_err(|err| Error::Device(err.localizedDescription().to_string()))?;
        }

        start_capture(&stream)?;

        Ok(Self {
            stream,
            output,
            _queue: queue,
            dropped,
            stopped: Mutex::new(false),
        })
    }

    pub(super) const fn sample_rate() -> i32 {
        SAMPLE_RATE
    }

    pub(super) const fn channels() -> i32 {
        CHANNELS
    }

    pub(super) fn dropped(&self) -> usize {
        self.dropped.load(Ordering::Relaxed)
    }

    pub(super) fn stop(&mut self) {
        let Ok(mut stopped) = self.stopped.lock() else {
            return;
        };
        if *stopped {
            return;
        }
        *stopped = true;

        // SCK completion handlers run on the main runloop. Stop must run on the
        // main thread and pump that runloop while waiting.
        if MainThreadMarker::new().is_some() {
            teardown_stream(&self.stream, &self.output);
        } else {
            let proto: &ProtocolObject<dyn SCStreamOutput> =
                ProtocolObject::from_ref(&*self.output);
            unsafe {
                let _ = self
                    .stream
                    .removeStreamOutput_type_error(proto, SCStreamOutputType::Audio);
                // Fire-and-forget: the completion cannot be awaited off the main
                // thread without deadlocking the caller.
                self.stream.stopCaptureWithCompletionHandler(None);
            }
        }
    }
}

impl Drop for MacSystemCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

struct OutputIvars {
    on_audio: AudioSink,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "OtoSCStreamAudioOutput"]
    #[ivars = OutputIvars]
    struct AudioOutput;

    unsafe impl NSObjectProtocol for AudioOutput {}

    unsafe impl SCStreamOutput for AudioOutput {
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        unsafe fn stream_did_output(
            &self,
            _stream: &SCStream,
            sample_buffer: &CMSampleBuffer,
            of_type: SCStreamOutputType,
        ) {
            if of_type != SCStreamOutputType::Audio {
                return;
            }
            if let Some(pcm) = extract_f32_pcm(sample_buffer) {
                (self.ivars().on_audio)(pcm);
            }
        }
    }
);

impl AudioOutput {
    fn new(on_audio: AudioSink) -> Retained<Self> {
        let this = Self::alloc().set_ivars(OutputIvars { on_audio });
        unsafe { msg_send![super(this), init] }
    }
}

/// Builds a whole-display filter capturing everything on the first display
/// (desktop, dock, and all apps) — i.e. the system's output mix.
///
/// `ScreenCaptureKit` audio needs at least one display; a headless session
/// reports zero displays, in which case there is nothing to mix.
fn make_system_filter(content: &SCShareableContent) -> Result<Retained<SCContentFilter>, Error> {
    let displays = unsafe { content.displays() };
    if displays.count() == 0 {
        return Err(Error::Device(
            "ScreenCaptureKit found no displays (headless session?)".to_owned(),
        ));
    }
    let display = displays.objectAtIndex(0);
    let excepting: Retained<NSArray<objc2_screen_capture_kit::SCWindow>> = NSArray::from_slice(&[]);
    Ok(unsafe {
        SCContentFilter::initWithDisplay_excludingWindows(
            SCContentFilter::alloc(),
            &display,
            &excepting,
        )
    })
}

fn make_audio_config() -> Retained<SCStreamConfiguration> {
    let config = unsafe { SCStreamConfiguration::new() };
    unsafe {
        config.setWidth(2);
        config.setHeight(2);
        config.setPixelFormat(PIXEL_FORMAT_BGRA);
        config.setMinimumFrameInterval(CMTime::new(1, 1));
        config.setQueueDepth(3);
        config.setCapturesAudio(true);
        config.setExcludesCurrentProcessAudio(false);
        config.setChannelCount(CHANNELS as isize);
        config.setSampleRate(SAMPLE_RATE as isize);
    }
    config
}

fn shareable_content() -> Result<Retained<SCShareableContent>, Error> {
    let (tx, rx) = std::sync::mpsc::channel();
    let block = RcBlock::new(move |content: *mut SCShareableContent, err: *mut NSError| {
        let result = if content.is_null() {
            let msg = unsafe { Retained::retain(err) }.map_or_else(
                || "ScreenCaptureKit content unavailable".into(),
                |e| e.localizedDescription().to_string(),
            );
            Err(Error::Device(msg))
        } else {
            unsafe { Retained::retain(content) }
                .ok_or_else(|| Error::Device("SCShareableContent retain failed".to_owned()))
        };
        let _ = tx.send(result);
    });
    unsafe {
        SCShareableContent::getShareableContentWithCompletionHandler(&block);
    }
    recv_pumping_runloop(&rx, SCK_WAIT_TIMEOUT)?
}

fn start_capture(stream: &SCStream) -> Result<(), Error> {
    let (tx, rx) = std::sync::mpsc::channel::<Result<(), String>>();
    let block = RcBlock::new(move |err: *mut NSError| {
        let result = if err.is_null() {
            Ok(())
        } else {
            Err(unsafe { Retained::retain(err) }.map_or_else(
                || "startCapture failed".into(),
                |e| e.localizedDescription().to_string(),
            ))
        };
        let _ = tx.send(result);
    });
    unsafe {
        stream.startCaptureWithCompletionHandler(Some(&block));
    }
    match recv_pumping_runloop(&rx, SCK_WAIT_TIMEOUT)? {
        Ok(()) => Ok(()),
        Err(msg) => Err(Error::Device(msg)),
    }
}

fn stop_capture(stream: &SCStream) {
    let (tx, rx) = std::sync::mpsc::channel::<Result<(), String>>();
    let block = RcBlock::new(move |err: *mut NSError| {
        let result = if err.is_null() {
            Ok(())
        } else {
            Err(unsafe { Retained::retain(err) }.map_or_else(
                || "stopCapture failed".into(),
                |e| e.localizedDescription().to_string(),
            ))
        };
        let _ = tx.send(result);
    });
    unsafe {
        stream.stopCaptureWithCompletionHandler(Some(&block));
    }
    // Best-effort: the completion may not fire if TCC was revoked mid-session.
    let _ = recv_pumping_runloop(&rx, SCK_STOP_TIMEOUT);
}

fn teardown_stream(
    stream: &SCStream,
    output: &AudioOutput,
) {
    let proto: &ProtocolObject<dyn SCStreamOutput> = ProtocolObject::from_ref(output);
    unsafe {
        let _ = stream.removeStreamOutput_type_error(proto, SCStreamOutputType::Audio);
    }
    stop_capture(stream);
}

fn recv_pumping_runloop<T>(
    rx: &std::sync::mpsc::Receiver<T>,
    timeout: Duration,
) -> Result<T, Error> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match rx.try_recv() {
            Ok(value) => return Ok(value),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                return Err(Error::Device(
                    "ScreenCaptureKit wait channel closed".to_owned(),
                ));
            },
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                if std::time::Instant::now() >= deadline {
                    return Err(Error::Device(
                        "ScreenCaptureKit timed out (runloop/TCC?)".to_owned(),
                    ));
                }
                unsafe {
                    let _ = CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.05, 1);
                }
            },
        }
    }
}

/// Copies the raw in-memory bytes of an interleaved Float32 buffer so the frame
/// can cross the channel boundary. The convert layer reads them back with
/// `f32::from_le_bytes`, which matches the host's native byte order.
fn bytes_of_f32(pcm: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(std::mem::size_of_val(pcm));
    // SAFETY: f32 has no invalid bit patterns; this is a plain byte copy.
    unsafe {
        out.extend_from_slice(std::slice::from_raw_parts(
            pcm.as_ptr().cast::<u8>(),
            std::mem::size_of_val(pcm),
        ));
    }
    out
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

#[link(name = "CoreMedia", kind = "framework")]
unsafe extern "C" {
    fn CMSampleBufferGetNumSamples(sbuf: *const CMSampleBuffer) -> i64;
    fn CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(
        sbuf: *const CMSampleBuffer,
        buffer_list_size_needed_out: *mut usize,
        buffer_list_out: *mut AudioBufferList,
        buffer_list_size: usize,
        block_buffer_structure_allocator: *const c_void,
        block_buffer_block_allocator: *const c_void,
        flags: u32,
        block_buffer_out: *mut *mut c_void,
    ) -> i32;
}

fn extract_f32_pcm(sample_buffer: &CMSampleBuffer) -> Option<Vec<f32>> {
    unsafe {
        let sbuf = std::ptr::from_ref(sample_buffer);
        let frames = CMSampleBufferGetNumSamples(sbuf);
        if frames <= 0 {
            return None;
        }

        let mut needed = 0usize;
        let st = CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(
            sbuf,
            &raw mut needed,
            ptr::null_mut(),
            0,
            ptr::null(),
            ptr::null(),
            0,
            ptr::null_mut(),
        );
        if st != 0 || needed == 0 {
            return None;
        }

        let layout = Layout::from_size_align(needed, 16).ok()?;
        let abl_ptr = alloc(layout).cast::<AudioBufferList>();
        if abl_ptr.is_null() {
            return None;
        }

        let mut block: *mut c_void = ptr::null_mut();
        let st = CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(
            sbuf,
            ptr::null_mut(),
            abl_ptr,
            needed,
            ptr::null(),
            ptr::null(),
            ASSURE_16_BYTE_ALIGNMENT,
            &raw mut block,
        );
        if st != 0 {
            dealloc(abl_ptr.cast(), layout);
            if !block.is_null() {
                CFRelease(block);
            }
            return None;
        }

        let abl = &*abl_ptr;
        let nbuf = abl.m_number_buffers as usize;
        let frames = frames as usize;
        let mut out = vec![0.0f32; frames.saturating_mul(CHANNELS as usize)];

        if nbuf == 1 {
            let b = &abl.m_buffers[0];
            let samples = (b.m_data_byte_size as usize) / 4;
            let src = std::slice::from_raw_parts(b.m_data.cast::<f32>(), samples);
            if b.m_number_channels <= 1 {
                for (i, &s) in src.iter().take(frames).enumerate() {
                    out[i * 2] = s;
                    out[i * 2 + 1] = s;
                }
            } else {
                let n = samples.min(out.len());
                out[..n].copy_from_slice(&src[..n]);
                out.truncate(n);
            }
        } else {
            for ch in 0..nbuf.min(2) {
                let b = &*abl.m_buffers.as_ptr().add(ch);
                let src = std::slice::from_raw_parts(
                    b.m_data.cast::<f32>(),
                    (b.m_data_byte_size as usize) / 4,
                );
                for (i, &s) in src.iter().take(frames).enumerate() {
                    out[i * 2 + ch] = s;
                }
            }
        }

        if !block.is_null() {
            CFRelease(block);
        }
        dealloc(abl_ptr.cast(), layout);
        Some(out)
    }
}

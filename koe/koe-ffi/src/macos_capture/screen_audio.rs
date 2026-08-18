//! Per-app system audio via ScreenCaptureKit.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_ptr_alignment,
    clippy::cast_sign_loss,
    clippy::needless_pass_by_value,
    clippy::non_send_fields_in_send_ty,
    clippy::unwrap_used
)]

use std::alloc::{Layout, alloc, dealloc};
use std::ffi::c_void;
use std::ptr;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use block2::RcBlock;
use dispatch2::{DispatchQueue, DispatchQueueAttr, DispatchRetained};
use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{AnyThread, DefinedClass, MainThreadMarker, Message, define_class, msg_send};
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
use objc2_core_media::{CMSampleBuffer, CMTime};
use objc2_foundation::{NSArray, NSError};
use objc2_screen_capture_kit::{
    SCContentFilter, SCRunningApplication, SCShareableContent, SCStream, SCStreamConfiguration,
    SCStreamOutput, SCStreamOutputType, SCWindow,
};

use crate::error::CaptureError;
use crate::handles::CaptureHandle;

use super::{CaptureSession, monotonic_ms};

/// 'BGRA' — minimal video plane (discarded; audio-only output registered).
const PIXEL_FORMAT_BGRA: u32 = u32::from_be_bytes(*b"BGRA");
const ASSURE_16_BYTE_ALIGNMENT: u32 = 1 << 0;
const SCK_WAIT_TIMEOUT: Duration = Duration::from_secs(15);
const SCK_STOP_TIMEOUT: Duration = Duration::from_secs(5);

type AudioSink = Arc<dyn Fn(Vec<f32>) + Send + Sync>;
type CFRunLoopRef = *mut c_void;
type CFStringRef = *const c_void;
type CFTimeInterval = f64;
type CFRunLoopMode = CFStringRef;
type Boolean = u8;

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRunLoopGetCurrent() -> CFRunLoopRef;
    fn CFRunLoopRunInMode(
        mode: CFRunLoopMode,
        seconds: CFTimeInterval,
        return_after_source_handled: Boolean,
    ) -> i32;
    static kCFRunLoopDefaultMode: CFStringRef;
    fn CFRelease(cf: *const c_void);
}

fn recv_pumping_runloop<T>(
    rx: &mpsc::Receiver<T>,
    timeout: Duration,
) -> Result<T, CaptureError> {
    let deadline = Instant::now() + timeout;
    loop {
        match rx.try_recv() {
            Ok(value) => return Ok(value),
            Err(mpsc::TryRecvError::Disconnected) => {
                return Err(CaptureError::Internal {
                    msg: "ScreenCaptureKit wait channel closed".to_owned(),
                });
            },
            Err(mpsc::TryRecvError::Empty) => {
                if Instant::now() >= deadline {
                    return Err(CaptureError::Internal {
                        msg: "ScreenCaptureKit timed out waiting for completion (runloop/TCC?)"
                            .to_owned(),
                    });
                }
                // SAFETY: pump current thread's runloop so SCK completions fire.
                unsafe {
                    let _ = CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.05, 1);
                    let _ = CFRunLoopGetCurrent();
                }
            },
        }
    }
}

struct OutputIvars {
    on_audio: AudioSink,
}

define_class!(
    // SAFETY: NSObject subclass with no Drop; ivars are Send+Sync Arc callbacks.
    #[unsafe(super(NSObject))]
    #[name = "KoeSCStreamAudioOutput"]
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

pub(super) struct ScreenAudioSession {
    stream: Retained<SCStream>,
    output: Retained<AudioOutput>,
    _queue: DispatchRetained<DispatchQueue>,
    stopped: Mutex<bool>,
}

// SAFETY: stop is synchronized; SCStream callbacks run on our serial queue.
unsafe impl Send for ScreenAudioSession {}

pub(super) fn start(
    bundle_id: &str,
    handle: Arc<CaptureHandle>,
) -> Result<Box<dyn CaptureSession>, CaptureError> {
    match start_sck(bundle_id, Arc::clone(&handle)) {
        Ok(session) => Ok(session),
        Err(sck_err) => {
            if let Some(pid) = pid_for_bundle(bundle_id)
                && let Ok(session) = super::process_tap::start(pid, Arc::clone(&handle))
            {
                return Ok(session);
            }
            // Last resort: global process mixdown (not app-filtered).
            match super::process_tap::start_global(handle) {
                Ok(session) => Ok(session),
                Err(tap_err) => Err(CaptureError::StreamError {
                    msg: format!(
                        "ScreenCaptureKit failed ({sck_err}); Process Tap also failed ({tap_err})"
                    ),
                }),
            }
        },
    }
}

fn pid_for_bundle(bundle_id: &str) -> Option<i32> {
    use objc2_app_kit::NSWorkspace;
    let workspace = NSWorkspace::sharedWorkspace();
    let running = workspace.runningApplications();
    for app in &running {
        let Some(id) = app.bundleIdentifier() else {
            continue;
        };
        if id.to_string() == bundle_id {
            let pid = app.processIdentifier();
            if pid > 0 {
                return Some(pid);
            }
        }
    }
    None
}

fn start_sck(
    bundle_id: &str,
    handle: Arc<CaptureHandle>,
) -> Result<Box<dyn CaptureSession>, CaptureError> {
    // ScreenCaptureKit / CoreGraphics require AppKit initialization in CLI hosts.
    let Some(mtm) = MainThreadMarker::new() else {
        return Err(CaptureError::Internal {
            msg: "ScreenCaptureKit must start on the main thread".to_owned(),
        });
    };
    let app = NSApplication::sharedApplication(mtm);
    let _ = app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    let content = shareable_content()?;
    let app = find_app(&content, bundle_id).ok_or_else(|| CaptureError::NoAudioSource {
        bundle_id: bundle_id.to_owned(),
    })?;
    let filter = make_filter(&content, &app)?;
    let config = make_audio_config();

    let weak = Arc::downgrade(&handle);
    drop(handle);
    let sink: AudioSink = Arc::new(move |pcm: Vec<f32>| {
        if let Some(handle) = weak.upgrade() {
            handle.deliver_audio(pcm, monotonic_ms());
        }
    });
    let output = AudioOutput::new(sink);
    let queue = DispatchQueue::new("dev.mokmok.koe.sck.audio", DispatchQueueAttr::SERIAL);

    let stream = unsafe {
        SCStream::initWithFilter_configuration_delegate(SCStream::alloc(), &filter, &config, None)
    };

    let proto: &ProtocolObject<dyn SCStreamOutput> = ProtocolObject::from_ref(&*output);
    unsafe {
        stream
            .addStreamOutput_type_sampleHandlerQueue_error(
                proto,
                SCStreamOutputType::Audio,
                Some(&queue),
            )
            .map_err(|err| CaptureError::StreamError {
                msg: err.localizedDescription().to_string(),
            })?;
    }

    start_capture(&stream)?;

    Ok(Box::new(ScreenAudioSession {
        stream,
        output,
        _queue: queue,
        stopped: Mutex::new(false),
    }))
}

fn shareable_content() -> Result<Retained<SCShareableContent>, CaptureError> {
    let (tx, rx) = mpsc::channel();
    let block = RcBlock::new(move |content: *mut SCShareableContent, err: *mut NSError| {
        let result = if content.is_null() {
            let msg = unsafe { Retained::retain(err) }
                .map(|e| e.localizedDescription().to_string())
                .unwrap_or_else(|| "ScreenCaptureKit content unavailable".into());
            Err(CaptureError::PermissionDenied(msg))
        } else {
            // Completion passes a +0 borrowed object; retain it for our ownership.
            unsafe { Retained::retain(content) }.ok_or_else(|| CaptureError::Internal {
                msg: "SCShareableContent retain failed".to_owned(),
            })
        };
        let _ = tx.send(result);
    });
    // SAFETY: completion invoked once on an arbitrary queue.
    unsafe {
        SCShareableContent::getShareableContentWithCompletionHandler(&block);
    }
    recv_pumping_runloop(&rx, SCK_WAIT_TIMEOUT)?
}

fn start_capture(stream: &SCStream) -> Result<(), CaptureError> {
    let (tx, rx) = mpsc::channel::<Result<(), String>>();
    let block = RcBlock::new(move |err: *mut NSError| {
        let result = if err.is_null() {
            Ok(())
        } else {
            Err(unsafe { Retained::retain(err) }
                .map(|e| e.localizedDescription().to_string())
                .unwrap_or_else(|| "startCapture failed".into()))
        };
        let _ = tx.send(result);
    });
    unsafe {
        stream.startCaptureWithCompletionHandler(Some(&block));
    }
    match recv_pumping_runloop(&rx, SCK_WAIT_TIMEOUT)? {
        Ok(()) => Ok(()),
        Err(msg) => Err(CaptureError::StreamError { msg }),
    }
}

fn teardown_stream(
    stream: &SCStream,
    output: &AudioOutput,
) {
    let proto: &ProtocolObject<dyn SCStreamOutput> = ProtocolObject::from_ref(output);
    // Best-effort detach so SCStream releases the output before stop completes.
    unsafe {
        let _ = stream.removeStreamOutput_type_error(proto, SCStreamOutputType::Audio);
    }
    stop_capture(stream);
}

fn stop_capture(stream: &SCStream) {
    let (tx, rx) = mpsc::channel::<Result<(), String>>();
    let block = RcBlock::new(move |err: *mut NSError| {
        let result = if err.is_null() {
            Ok(())
        } else {
            Err(unsafe { Retained::retain(err) }
                .map(|e| e.localizedDescription().to_string())
                .unwrap_or_else(|| "stopCapture failed".into()))
        };
        let _ = tx.send(result);
    });
    unsafe {
        stream.stopCaptureWithCompletionHandler(Some(&block));
    }
    // Best-effort: completion may not fire if TCC revoked mid-session.
    let _ = recv_pumping_runloop(&rx, SCK_STOP_TIMEOUT);
}

fn find_app(
    content: &SCShareableContent,
    bundle_id: &str,
) -> Option<Retained<SCRunningApplication>> {
    let apps = unsafe { content.applications() };
    for app in apps {
        let id = unsafe { app.bundleIdentifier() };
        if id.to_string() == bundle_id {
            return Some((*app).retain());
        }
    }
    None
}

fn make_filter(
    content: &SCShareableContent,
    app: &SCRunningApplication,
) -> Result<Retained<SCContentFilter>, CaptureError> {
    let displays = unsafe { content.displays() };
    if displays.count() > 0 {
        let display = displays.objectAtIndex(0);
        let including = NSArray::from_slice(&[app]);
        let excepting: Retained<NSArray<SCWindow>> = NSArray::from_slice(&[]);
        return Ok(unsafe {
            SCContentFilter::initWithDisplay_includingApplications_exceptingWindows(
                SCContentFilter::alloc(),
                &display,
                &including,
                &excepting,
            )
        });
    }

    // Some CLI contexts report apps/windows but zero displays. Fall back to a
    // window owned by the target app.
    let target_pid = unsafe { app.processID() };
    let windows = unsafe { content.windows() };
    let window_count = windows.count();
    for window in &windows {
        let Some(owner) = (unsafe { window.owningApplication() }) else {
            continue;
        };
        if unsafe { owner.processID() } == target_pid {
            return Ok(unsafe {
                SCContentFilter::initWithDesktopIndependentWindow(SCContentFilter::alloc(), &window)
            });
        }
    }

    Err(CaptureError::StreamError {
        msg: format!(
            "no display/window available for ScreenCaptureKit (displays=0 apps={} windows={})",
            unsafe { content.applications() }.count(),
            window_count
        ),
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
        config.setExcludesCurrentProcessAudio(true);
        config.setChannelCount(2);
        config.setSampleRate(48_000);
    }
    config
}

impl CaptureSession for ScreenAudioSession {
    fn stop(&mut self) {
        let Ok(mut stopped) = self.stopped.lock() else {
            return;
        };
        if *stopped {
            return;
        }
        *stopped = true;

        // ScreenCaptureKit completion handlers run on the main runloop. Stop
        // must run on the main thread and pump that runloop while waiting.
        if MainThreadMarker::new().is_some() {
            teardown_stream(&self.stream, &self.output);
        } else {
            // Best-effort when a multi-thread runtime calls stop from a worker.
            // CLI hosts should call `RecordingPipeline::stop_native_captures` on
            // the main thread before async `stop` (see `koe record`).
            let proto: &ProtocolObject<dyn SCStreamOutput> =
                ProtocolObject::from_ref(&*self.output);
            unsafe {
                let _ = self
                    .stream
                    .removeStreamOutput_type_error(proto, SCStreamOutputType::Audio);
                // SAFETY: Fire-and-forget stop; completion cannot be awaited off
                // the main thread without deadlocking `block_on` on the main thread.
                self.stream.stopCaptureWithCompletionHandler(None);
            }
        }
    }
}

impl Drop for ScreenAudioSession {
    fn drop(&mut self) {
        self.stop();
    }
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
        let mut out = vec![0.0f32; frames.saturating_mul(2)];

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

import CoreMedia
import Foundation
import QuartzCore
import ScreenCaptureKit

/// Captures audio from a specific application using ScreenCaptureKit.
///
/// SCK is used exclusively for its audio capabilities: a stream is opened with
/// a minimal 1×1 video configuration and audio enabled, and the video frames
/// are discarded immediately. Audio is delivered by ``SCStreamOutput`` on a
/// serial dispatch queue (not a real-time audio thread), so the callback is
/// allowed to allocate, but should still do as little work as possible.
///
/// The stream is configured by the caller (48 kHz, stereo, Float32 is the
/// canonical Koe format); no post-hoc resampling is performed here.
///
/// Lifecycle: ``stop()`` must be called before the capture object is
/// released — while running, `SCStream` retains this object as an output and
/// this object retains the stream, so the pair is only released at ``stop()``.
public final class ScreenAudioCapture: NSObject, SCStreamOutput {
  /// Errors that can occur during capture lifecycle.
  public enum Error: Swift.Error {
    case permissionDenied
    case noAudioContent
    case streamSetupFailed(String)
    case streamStartFailed(String)
    case alreadyCapturing
  }

  /// A chunk of captured audio in the canonical format.
  public struct AudioBuffer {
    /// Interleaved Float32 PCM samples (format per stream configuration).
    public let samples: [Float]
    /// Number of frames (per channel) in `samples`.
    public let frameCount: Int
    /// Monotonic timestamp in milliseconds (used for stream alignment).
    public let timestampMilliseconds: UInt64
  }

  /// Callback type for receiving captured audio chunks.
  public typealias AudioBufferCallback = (AudioBuffer) -> Void

  /// Serial queue on which SCK invokes the output handler.
  private let sampleHandlerQueue = DispatchQueue(
    label: "koe.screen-audio-capture.samples"
  )

  /// The active stream (nil when idle).
  private var stream: SCStream?

  /// Client callback; set before the stream starts.
  private var callback: AudioBufferCallback?

  /// Returns the currently shareable content (displays, apps, windows).
  ///
  /// Throws if Screen Recording permission has not been granted.
  public static func enumerateContent() async throws -> SCShareableContent {
    try await SCShareableContent.current
  }

  /// Starts capturing audio from a specific application.
  ///
  /// - Parameters:
  ///   - target: The application to capture.
  ///   - config: Stream configuration. `capturesAudio` must be enabled;
  ///     the delivered PCM format follows `channelCount`/`sampleRate`.
  ///   - callback: Receives captured audio chunks on a serial queue.
  /// - Throws: ``Error/alreadyCapturing`` if already running,
  ///   ``Error/noAudioContent`` if the configuration disables audio or no
  ///   display is available, or a stream setup/start failure.
  public func start(
    target: SCRunningApplication,
    config: SCStreamConfiguration,
    callback: @escaping AudioBufferCallback
  ) async throws {
    guard stream == nil else { throw Error.alreadyCapturing }
    guard config.capturesAudio else { throw Error.noAudioContent }

    let content: SCShareableContent
    do {
      content = try await SCShareableContent.current
    } catch {
      if let nsError = error as NSError?,
        nsError.domain == SCStreamErrorDomain,
        nsError.code == SCStreamError.userDeclined.rawValue
      {
        throw Error.permissionDenied
      }
      throw error
    }
    guard let display = content.displays.first else {
      throw Error.noAudioContent
    }

    let filter = SCContentFilter(
      display: display,
      including: [target],
      exceptingWindows: []
    )

    let newStream = SCStream(
      filter: filter,
      configuration: config,
      delegate: nil
    )
    self.callback = callback

    do {
      try newStream.addStreamOutput(
        self,
        type: .audio,
        sampleHandlerQueue: sampleHandlerQueue
      )
    } catch {
      self.callback = nil
      throw Error.streamSetupFailed(error.localizedDescription)
    }

    do {
      try await newStream.startCapture()
    } catch {
      self.callback = nil
      throw Error.streamStartFailed(error.localizedDescription)
    }

    stream = newStream
  }

  /// Stops the capture session and releases the stream. Safe to call multiple
  /// times.
  public func stop() async {
    guard let stream else { return }
    do {
      try await stream.stopCapture()
    } catch {
      // Best-effort teardown; nothing left to do.
    }
    self.stream = nil
    callback = nil
  }

  // MARK: - SCStreamOutput

  public func stream(
    _ stream: SCStream,
    didOutputSampleBuffer sampleBuffer: CMSampleBuffer,
    of type: SCStreamOutputType
  ) {
    guard type == .audio else { return }  // Discard video frames immediately.

    guard
      let formatDesc = CMSampleBufferGetFormatDescription(sampleBuffer),
      let asbd = CMAudioFormatDescriptionGetStreamBasicDescription(formatDesc)
    else { return }

    // Extract the PCM data (contiguous, 16-byte aligned).
    var bufferListSize = 0
    var blockBuffer: CMBlockBuffer?
    let status = CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(
      sampleBuffer,
      bufferListSizeNeededOut: &bufferListSize,
      bufferListOut: nil,
      bufferListSize: 0,
      blockBufferAllocator: nil,
      blockBufferMemoryAllocator: nil,
      flags: kCMSampleBufferFlag_AudioBufferList_Assure16ByteAlignment,
      blockBufferOut: &blockBuffer
    )
    guard status == noErr, bufferListSize > 0 else { return }

    let memory = UnsafeMutableRawPointer.allocate(
      byteCount: bufferListSize,
      alignment: MemoryLayout<AudioBufferList>.alignment
    )
    defer { memory.deallocate() }
    let bufferList = memory.bindMemory(to: AudioBufferList.self, capacity: 1)

    let fillStatus = CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(
      sampleBuffer,
      bufferListSizeNeededOut: &bufferListSize,
      bufferListOut: bufferList,
      bufferListSize: bufferListSize,
      blockBufferAllocator: nil,
      blockBufferMemoryAllocator: nil,
      flags: kCMSampleBufferFlag_AudioBufferList_Assure16ByteAlignment,
      blockBufferOut: &blockBuffer
    )
    guard fillStatus == noErr else { return }

    let buffers = UnsafeMutableAudioBufferListPointer(bufferList)
    let frameCount = Int(CMSampleBufferGetNumSamples(sampleBuffer))
    let samplesPerFrame = Int(asbd.pointee.mChannelsPerFrame)
    var totalSamples = 0
    for index in 0..<buffers.count {
      totalSamples +=
        Int(buffers[index].mDataByteSize) / MemoryLayout<Float>.size
    }
    guard totalSamples > 0, samplesPerFrame > 0 else { return }

    // Copy PCM into an owned, canonical-shaped value for the client.
    var samples = [Float](repeating: 0, count: totalSamples)
    samples.withUnsafeMutableBufferPointer { destination in
      guard let base = destination.baseAddress else { return }
      var writeOffset = 0
      for index in 0..<buffers.count {
        let buffer = buffers[index]
        guard let data = buffer.mData else { continue }
        let count = Int(buffer.mDataByteSize) / MemoryLayout<Float>.size
        (base + writeOffset).update(
          from: data.assumingMemoryBound(to: Float.self),
          count: count
        )
        writeOffset += count
      }
    }

    callback?(
      AudioBuffer(
        samples: samples,
        frameCount: frameCount,
        timestampMilliseconds: UInt64(CACurrentMediaTime() * 1_000)
      )
    )
  }
}

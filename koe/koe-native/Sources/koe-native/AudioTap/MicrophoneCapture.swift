import AudioToolbox
import Foundation
import os

/// Captures microphone input via an `AudioQueue` input queue.
///
/// The queue is configured with the canonical Koe format (48 kHz, stereo,
/// Float32, interleaved); AudioQueue handles conversion from the actual
/// input device format (sample rate, bit depth, and mono→stereo upmix), so
/// every delivered buffer is already canonical.
///
/// The input callback runs on one of the queue's internal threads (not a
/// hard real-time audio thread), but it still does minimal work: it forwards
/// the buffer to the client callback and refreshes the RMS level.
///
/// Threading model: `init`, `start(callback:)`, and `currentLevel` are used
/// from the owning thread; `stop()` may be called from any thread.
/// `AudioQueueStop(_:true:)` is synchronous, so after it returns no input
/// callback is running and teardown is race-free.
public final class MicrophoneCapture: @unchecked Sendable {
  /// Errors that can occur during capture lifecycle.
  public enum Error: Swift.Error {
    case permissionDenied
    case queueCreationFailed(OSStatus)
    case queueStartFailed(OSStatus)
    case alreadyRunning
  }

  /// Number of 20 ms host buffers kept in flight (frames = 960 at 48 kHz).
  private static let bufferFrameCount = 960
  private static let bufferCount = 3

  /// Canonical input format: 48 kHz, stereo, Float32, interleaved.
  private static let canonicalFormat: AudioStreamBasicDescription = {
    var format = AudioStreamBasicDescription()
    format.mSampleRate = 48_000
    format.mFormatID = kAudioFormatLinearPCM
    format.mFormatFlags = kAudioFormatFlagIsFloat | kAudioFormatFlagIsPacked
    format.mBitsPerChannel = 32
    format.mChannelsPerFrame = 2
    format.mFramesPerPacket = 1
    format.mBytesPerFrame = 8
    format.mBytesPerPacket = 8
    return format
  }()

  /// Callback type for receiving canonical PCM data.
  ///
  /// - Parameters:
  ///   - pcm: Interleaved Float32 PCM (48 kHz, stereo).
  ///   - frameCount: Number of frames (per channel) in `pcm`.
  public typealias MicrophoneCallback = (UnsafePointer<Float>, Int) -> Void

  private var queue: AudioQueueRef?
  private var buffers: [AudioQueueBufferRef] = []
  private var isRunning = false

  /// Client callback; set before the queue starts.
  private var callback: MicrophoneCallback?

  /// Latest RMS level (0.0–1.0 linear), written by the callback thread and
  /// polled by the consumer. `OSAllocatedUnfairLock` keeps the read/write
  /// race-free without blocking for meaningful periods.
  private let levelLock = OSAllocatedUnfairLock(initialState: Float(0))

  /// Initializes the capture queue.
  ///
  /// - Throws: ``Error/permissionDenied`` if microphone access has not been
  ///   granted, or ``Error/queueCreationFailed(_:)`` if the queue could not
  ///   be created.
  public init() throws {
    guard PermissionChecker.status(for: .microphone) == .authorized else {
      throw Error.permissionDenied
    }

    var queue: AudioQueueRef?
    var format = MicrophoneCapture.canonicalFormat
    let status = AudioQueueNewInput(
      &format,
      MicrophoneCapture.inputCallback,
      Unmanaged.passUnretained(self).toOpaque(),
      nil,
      nil,
      0,
      &queue
    )
    guard status == noErr, let queue else {
      throw Error.queueCreationFailed(status)
    }

    self.queue = queue
  }

  deinit {
    stop()
  }

  /// Starts capturing microphone input.
  ///
  /// - Parameter callback: Receives canonical PCM frames.
  /// - Throws: ``Error/alreadyRunning`` if already running, or
  ///   ``Error/queueStartFailed(_:)`` if the queue could not be started.
  public func start(callback: @escaping MicrophoneCallback) throws {
    guard !isRunning else { throw Error.alreadyRunning }
    guard let queue else { throw Error.queueCreationFailed(kAudioQueueErr_InvalidRunState) }

    self.callback = callback
    isRunning = true

    let bufferByteSize = UInt32(
      MicrophoneCapture.bufferFrameCount
        * Int(MicrophoneCapture.canonicalFormat.mBytesPerFrame)
    )
    for _ in 0..<MicrophoneCapture.bufferCount {
      var buffer: AudioQueueBufferRef?
      let status = AudioQueueAllocateBuffer(queue, bufferByteSize, &buffer)
      guard status == noErr, let buffer else {
        stop()
        throw Error.queueStartFailed(status)
      }
      AudioQueueEnqueueBuffer(queue, buffer, 0, nil)
      buffers.append(buffer)
    }

    let startStatus = AudioQueueStart(queue, nil)
    guard startStatus == noErr else {
      isRunning = false
      stop()
      throw Error.queueStartFailed(startStatus)
    }
  }

  /// Stops capturing and releases the queue's buffers. Safe to call multiple
  /// times.
  public func stop() {
    guard let queue else { return }

    AudioQueueStop(queue, true)
    for buffer in buffers {
      AudioQueueFreeBuffer(queue, buffer)
    }
    buffers.removeAll()
    callback = nil
    isRunning = false

    AudioQueueDispose(queue, false)
    self.queue = nil
  }

  /// The most recent RMS level of the input, in linear 0.0–1.0. Polled by the
  /// consumer (e.g. for GUI level meters at ~60 Hz).
  public var currentLevel: Float {
    levelLock.withLock { $0 }
  }

  // MARK: - AudioQueue callback

  private static let inputCallback: AudioQueueInputCallback = {
    userData,
    _queue,
    buffer,
    _startTime,
    _packetCount,
    _packetDescs in
    guard let userData else { return }
    let capture = Unmanaged<MicrophoneCapture>.fromOpaque(userData)
      .takeUnretainedValue()
    capture.handleInput(buffer: buffer)
  }

  private func handleInput(buffer: AudioQueueBufferRef) {
    let byteCount = Int(buffer.pointee.mAudioDataByteSize)
    guard byteCount >= MemoryLayout<Float>.size else { return }

    let frameCount =
      byteCount
      / Int(MicrophoneCapture.canonicalFormat.mBytesPerFrame)
    guard frameCount > 0 else { return }

    // Refresh the level meter from the most recent buffer.
    let sampleCount = byteCount / MemoryLayout<Float>.size
    let samples = buffer.pointee.mAudioData.assumingMemoryBound(to: Float.self)
    var sumSquares: Float = 0
    for index in 0..<sampleCount {
      let sample = samples[index]
      sumSquares += sample * sample
    }
    let rms = (sumSquares / Float(sampleCount)).squareRoot()
    levelLock.withLock { $0 = rms }

    // Re-enqueue the buffer so capture continues. Done before the client
    // callback so buffers keep flowing even if the callback is slow.
    if let queue {
      AudioQueueEnqueueBuffer(queue, buffer, 0, nil)
    }

    callback?(samples, frameCount)
  }

}

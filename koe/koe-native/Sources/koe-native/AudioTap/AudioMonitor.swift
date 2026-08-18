import AudioToolbox
import Foundation
import os

/// Plays clean (post-AEC) PCM to the default output device for live monitoring.
///
/// Configured for the canonical Koe format (48 kHz, stereo, Float32,
/// interleaved) with a single 20 ms host buffer (~960 frames) to keep
/// pass-through latency near the spec budget (~15–35 ms end-to-end).
///
/// Producer threads call ``write(_:)``; the AudioQueue output callback drains
/// a small lock-protected pending buffer. Underruns play silence so the queue
/// keeps running without glitching the rest of the pipeline.
///
/// Threading: `start()`, `write(_:)`, and `stop()` are safe from any thread.
/// `AudioQueueStop(_:true:)` is synchronous, so after `stop()` returns no
/// output callback is running.
public final class AudioMonitor: @unchecked Sendable {
  /// Errors that can occur during monitor lifecycle.
  public enum Error: Swift.Error {
    case queueCreationFailed(OSStatus)
    case queueStartFailed(OSStatus)
    case alreadyRunning
    case notRunning
  }

  /// One 20 ms host buffer at 48 kHz (spec: 1 × 20 ms for minimal latency).
  private static let bufferFrameCount = 960
  private static let bufferCount = 1

  /// Canonical output format: 48 kHz, stereo, Float32, interleaved.
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

  private var queue: AudioQueueRef?
  private var buffers: [AudioQueueBufferRef] = []
  private var isRunning = false

  /// Pending interleaved samples waiting for the output callback.
  private let pendingLock = OSAllocatedUnfairLock(initialState: [Float]())

  /// Creates an output AudioQueue in the canonical format.
  ///
  /// - Throws: ``Error/queueCreationFailed(_:)`` if the queue could not be created.
  public init() throws {
    var queue: AudioQueueRef?
    var format = AudioMonitor.canonicalFormat
    let status = AudioQueueNewOutput(
      &format,
      AudioMonitor.outputCallback,
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

  /// Starts playback to the default output device.
  ///
  /// - Throws: ``Error/alreadyRunning`` if already running, or
  ///   ``Error/queueStartFailed(_:)`` if the queue could not be started.
  public func start() throws {
    guard !isRunning else { throw Error.alreadyRunning }
    guard let queue else { throw Error.queueCreationFailed(kAudioQueueErr_InvalidRunState) }

    let bufferByteSize = UInt32(
      AudioMonitor.bufferFrameCount
        * Int(AudioMonitor.canonicalFormat.mBytesPerFrame)
    )
    for _ in 0..<AudioMonitor.bufferCount {
      var buffer: AudioQueueBufferRef?
      let status = AudioQueueAllocateBuffer(queue, bufferByteSize, &buffer)
      guard status == noErr, let buffer else {
        stop()
        throw Error.queueStartFailed(status)
      }
      // Prime with silence so the first callback has a valid enqueued buffer.
      buffer.pointee.mAudioDataByteSize = bufferByteSize
      let sampleCount = AudioMonitor.bufferFrameCount * 2
      let samples = buffer.pointee.mAudioData.assumingMemoryBound(to: Float.self)
      for index in 0..<sampleCount {
        samples[index] = 0
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
    isRunning = true
  }

  /// Enqueues interleaved stereo Float32 PCM for playback.
  ///
  /// Non-blocking from the caller's perspective: samples are copied into the
  /// pending buffer and drained by the AudioQueue callback.
  ///
  /// - Parameter pcm: Interleaved Float32 PCM (48 kHz, stereo).
  /// - Throws: ``Error/notRunning`` if `start()` has not been called.
  public func write(_ pcm: [Float]) throws {
    guard isRunning else { throw Error.notRunning }
    pendingLock.withLock { pending in
      pending.append(contentsOf: pcm)
      // Cap pending audio to ~200 ms to bound memory if the device stalls.
      let maxSamples = AudioMonitor.bufferFrameCount * 2 * 10
      if pending.count > maxSamples {
        pending.removeFirst(pending.count - maxSamples)
      }
    }
  }

  /// Stops playback and releases the queue. Safe to call multiple times.
  public func stop() {
    guard let queue else { return }

    AudioQueueStop(queue, true)
    for buffer in buffers {
      AudioQueueFreeBuffer(queue, buffer)
    }
    buffers.removeAll()
    pendingLock.withLock { $0.removeAll(keepingCapacity: false) }
    isRunning = false

    AudioQueueDispose(queue, false)
    self.queue = nil
  }

  // MARK: - AudioQueue callback

  private static let outputCallback: AudioQueueOutputCallback = {
    userData,
    queue,
    buffer in
    guard let userData else { return }
    let monitor = Unmanaged<AudioMonitor>.fromOpaque(userData)
      .takeUnretainedValue()
    monitor.handleOutput(queue: queue, buffer: buffer)
  }

  private func handleOutput(
    queue: AudioQueueRef,
    buffer: AudioQueueBufferRef
  ) {
    let capacityBytes = Int(buffer.pointee.mAudioDataBytesCapacity)
    let sampleCapacity = capacityBytes / MemoryLayout<Float>.size
    let samples = buffer.pointee.mAudioData.assumingMemoryBound(to: Float.self)

    let filled = pendingLock.withLock { pending -> Int in
      let count = min(sampleCapacity, pending.count)
      if count > 0 {
        for index in 0..<count {
          samples[index] = pending[index]
        }
        pending.removeFirst(count)
      }
      return count
    }

    // Pad underruns with silence so the queue keeps running.
    if filled < sampleCapacity {
      for index in filled..<sampleCapacity {
        samples[index] = 0
      }
    }

    buffer.pointee.mAudioDataByteSize = UInt32(sampleCapacity * MemoryLayout<Float>.size)
    AudioQueueEnqueueBuffer(queue, buffer, 0, nil)
  }
}

import AudioToolbox
import CoreAudio
import Foundation

/// Captures the audio output of a single process by installing a CoreAudio
/// process tap (macOS 14.2+ public API).
///
/// The tap is inserted into the target process's audio output pipeline before
/// it reaches the hardware. Audio continues to flow to the output device
/// uninterrupted (the tap is unmuted); we receive a copy of each buffer on the
/// real-time audio thread.
///
/// The tap delivers a stereo mixdown of the process's output at the sample
/// rate of its output device. ``start(callback:)`` hands back canonically
/// normalized (48 kHz, stereo, Float32, interleaved) frames via the callback.
///
/// Threading model:
/// - `init(pid:)`, `start(callback:)`, and `status` are used from the owning
///   thread.
/// - `stop()` may be called from any thread; it tears down the tap and stops
///   the real-time callback.
/// - The real-time IO proc only reads state that is fully set up before
///   `start()` returns (normalizer, scratch buffer, callback) and is never
///   mutated while the tap is running, so the class is safe to share across
///   threads.
@available(macOS 14.2, *)
public final class AudioTap: @unchecked Sendable {
  /// Errors that can occur during tap lifecycle.
  public enum Error: Swift.Error {
    case processNotFound
    case tapCreationFailed(OSStatus)
    case tapStartFailed(OSStatus)
    case alreadyRunning
  }

  /// Status of the audio tap.
  public enum Status: Equatable {
    case idle
    case running
    case error
  }

  /// Callback type for receiving normalized audio data.
  ///
  /// Invoked on the real-time audio thread. The implementation MUST NOT
  /// allocate, acquire locks, perform I/O, or send Objective-C messages. It
  /// should copy `buffer` into a pre-allocated ring buffer and return.
  ///
  /// - Parameters:
  ///   - buffer: Canonical Float32 PCM (48 kHz, stereo, interleaved).
  ///   - frameCount: Number of frames (per channel) in `buffer`.
  ///   - streamDesc: Canonical stream format of `buffer`.
  public typealias AudioTapCallback = (
    _ buffer: UnsafePointer<Float>,
    _ frameCount: Int,
    _ streamDesc: AudioStreamBasicDescription
  ) -> Void

  /// Raw value of the deprecated `kAudioDevicePropertyStreamFormat` selector
  /// (`'sfmt'`). Defined locally so the tap's stream format can be queried
  /// without referencing the deprecated symbol.
  private static let streamFormatSelector: AudioObjectPropertySelector = 0x7366_6D74

  /// Target process ID.
  private let pid: pid_t

  /// The AudioObjectID of the installed tap (nil until created).
  private var tapID: AudioObjectID?

  /// IO procedure handle used to receive tapped audio.
  private var ioProcID: AudioDeviceIOProcID?

  /// Resamples the tap's native format into the canonical format.
  private var normalizer: FormatNormalizer?

  /// Pre-allocated buffer (canonical format) reused on the real-time thread.
  private var scratchBuffer: UnsafeMutablePointer<Float>?
  private var scratchCapacityFrames: Int = 0

  /// Client callback; set before the tap starts, read only on the real-time
  /// thread while running.
  private var callback: AudioTapCallback?

  /// Current lifecycle status.
  private var statusValue: Status = .idle

  /// Initializes a tap for the given process.
  ///
  /// - Parameter pid: The PID of the process whose audio output to capture.
  /// - Throws: ``Error/processNotFound`` if the process has no audio object,
  ///   or ``Error/tapCreationFailed(_:)`` if the tap could not be installed.
  public init(pid: pid_t) throws {
    self.pid = pid
    try createTap()
  }

  deinit {
    stop()
    scratchBuffer?.deinitialize(count: scratchCapacityFrames * 2)
    scratchBuffer?.deallocate()
    scratchBuffer = nil
  }

  /// Starts capturing audio from the target process.
  ///
  /// - Parameter callback: Receives normalized PCM frames on the real-time
  ///   audio thread.
  /// - Throws: ``Error/alreadyRunning`` if already running, or
  ///   ``Error/tapStartFailed(_:)`` if the IO proc could not be started.
  public func start(callback: @escaping AudioTapCallback) throws {
    guard statusValue != .running else { throw Error.alreadyRunning }
    guard let tapID else { throw Error.processNotFound }

    // Prepare the real-time path before the IO proc starts firing.
    prepareScratchBuffer()
    self.callback = callback

    var ioProc: AudioDeviceIOProcID?
    let createStatus = AudioDeviceCreateIOProcID(
      tapID,
      AudioTap.ioProc,
      Unmanaged.passUnretained(self).toOpaque(),
      &ioProc
    )
    guard createStatus == noErr else {
      statusValue = .error
      throw Error.tapStartFailed(createStatus)
    }
    ioProcID = ioProc

    let startStatus = AudioDeviceStart(tapID, ioProc)
    guard startStatus == noErr else {
      if let ioProc {
        AudioDeviceDestroyIOProcID(tapID, ioProc)
      }
      ioProcID = nil
      statusValue = .error
      throw Error.tapStartFailed(startStatus)
    }

    statusValue = .running
  }

  /// Stops the audio tap and releases its resources. Safe to call multiple
  /// times and from any thread.
  public func stop() {
    guard let tapID else { return }

    if let ioProcID {
      AudioDeviceStop(tapID, ioProcID)
      AudioDeviceDestroyIOProcID(tapID, ioProcID)
      self.ioProcID = nil
    }

    AudioHardwareDestroyProcessTap(tapID)
    self.tapID = nil
    callback = nil
    statusValue = .idle
  }

  /// The current status of the tap.
  public var status: Status { statusValue }

  // MARK: - Setup

  /// Translates the PID to a CoreAudio process object and installs a stereo
  /// mixdown tap on it.
  private func createTap() throws {
    guard let processID = AudioTap.processObject(for: pid) else {
      statusValue = .error
      throw Error.processNotFound
    }

    let description = CATapDescription(__stereoMixdownOfProcesses: [
      NSNumber(value: processID)
    ])
    description.name = "Koe Process Tap (\(pid))"

    var tapID: AudioObjectID = kAudioObjectUnknown
    let status = AudioHardwareCreateProcessTap(description, &tapID)
    guard status == noErr, tapID != kAudioObjectUnknown else {
      statusValue = .error
      throw Error.tapCreationFailed(status)
    }

    self.tapID = tapID

    do {
      if let sourceDesc = AudioTap.streamFormat(of: tapID) {
        normalizer = try FormatNormalizer(sourceDesc: sourceDesc)
      }
    } catch {
      // `deinit` does not run when `init` throws, so destroy the tap here
      // to avoid leaking it until process exit.
      AudioHardwareDestroyProcessTap(tapID)
      self.tapID = nil
      statusValue = .error
      throw error
    }
  }

  /// Pre-allocates the normalized scratch buffer used by the IO proc.
  /// Called once before the IO proc starts; never on the real-time thread.
  private func prepareScratchBuffer() {
    guard scratchBuffer == nil else { return }
    let capacityFrames = 4096
    scratchBuffer = UnsafeMutablePointer<Float>.allocate(
      capacity: capacityFrames * 2
    )
    scratchBuffer?.initialize(repeating: 0, count: capacityFrames * 2)
    scratchCapacityFrames = capacityFrames
  }

  // MARK: - Real-time IO proc

  /// C-callable IO proc invoked on the real-time audio thread.
  ///
  /// Does exactly three things, in order:
  /// 1. Pass the tapped audio through to the output (memcpy).
  /// 2. Normalize the tapped audio into the pre-allocated scratch buffer.
  /// 3. Invoke the client callback with the normalized frames.
  ///
  /// No allocation, no locks, no I/O.
  private static let ioProc: AudioDeviceIOProc = {
    _device,
    _now,
    inputData,
    _inputTime,
    outputData,
    _outputTime,
    clientData
    in
    guard let clientData else { return noErr }
    let tap = Unmanaged<AudioTap>.fromOpaque(clientData).takeUnretainedValue()
    tap.ioProc(
      inputData: inputData,
      outputData: outputData
    )
    return noErr
  }

  private func ioProc(
    inputData: UnsafePointer<AudioBufferList>?,
    outputData: UnsafeMutablePointer<AudioBufferList>?
  ) {
    guard let inputData else { return }

    // 1. Passthrough: tapped audio must be forwarded to the hardware output.
    if let outputData {
      copyBuffers(inputData, to: outputData)
    }

    // 2. Normalize into the pre-allocated scratch buffer.
    guard
      let normalizer,
      let scratchBuffer,
      let sourceBuffer = firstAudioBuffer(inputData),
      let sourceData = sourceBuffer.mData
    else { return }

    // CoreAudio process taps deliver their mixdown as interleaved Float32
    // PCM, so treating the data as `Float` is safe.
    let sourceFrames =
      Int(sourceBuffer.mDataByteSize)
      / Int(normalizer.sourceASBD.mBytesPerFrame)
    guard sourceFrames > 0 else { return }

    let source = sourceData.assumingMemoryBound(to: Float.self)
    let frameCount = normalizer.convert(
      input: source,
      frameCount: sourceFrames,
      output: scratchBuffer,
      maxOutputFrames: scratchCapacityFrames
    )
    guard frameCount > 0 else { return }

    // 3. Deliver to the client.
    callback?(scratchBuffer, frameCount, FormatNormalizer.canonicalASBD)
  }

  /// Copies every input buffer into the corresponding output buffer
  /// (byte-for-byte passthrough).
  private func copyBuffers(
    _ input: UnsafePointer<AudioBufferList>,
    to output: UnsafeMutablePointer<AudioBufferList>
  ) {
    let inputBuffers = UnsafeMutableAudioBufferListPointer(
      UnsafeMutablePointer(mutating: input)
    )
    let outputBuffers = UnsafeMutableAudioBufferListPointer(output)
    let count = min(inputBuffers.count, outputBuffers.count)

    for index in 0..<count {
      let source = inputBuffers[index]
      let destination = outputBuffers[index]
      guard
        let sourceData = source.mData,
        let destinationData = destination.mData,
        source.mDataByteSize > 0
      else { continue }
      let byteCount = Int(min(source.mDataByteSize, destination.mDataByteSize))
      destinationData.copyMemory(from: sourceData, byteCount: byteCount)
      outputBuffers[index].mDataByteSize = UInt32(byteCount)
    }
  }

  /// Returns the first non-empty audio buffer in the list.
  private func firstAudioBuffer(_ list: UnsafePointer<AudioBufferList>)
    -> AudioBuffer?
  {
    let buffers = UnsafeMutableAudioBufferListPointer(
      UnsafeMutablePointer(mutating: list)
    )
    for index in 0..<buffers.count {
      let buffer = buffers[index]
      if buffer.mData != nil && buffer.mDataByteSize > 0 {
        return buffer
      }
    }
    return nil
  }

  // MARK: - CoreAudio helpers

  /// Translates a PID to a CoreAudio process object ID.
  static func processObject(for pid: pid_t) -> AudioObjectID? {
    var pid = pid
    var address = AudioObjectPropertyAddress(
      mSelector: kAudioHardwarePropertyTranslatePIDToProcessObject,
      mScope: kAudioObjectPropertyScopeGlobal,
      mElement: kAudioObjectPropertyElementMain
    )

    var processObject: AudioObjectID = kAudioObjectUnknown
    var size = UInt32(MemoryLayout<AudioObjectID>.size)
    let status = AudioObjectGetPropertyData(
      AudioObjectID(kAudioObjectSystemObject),
      &address,
      UInt32(MemoryLayout<pid_t>.size),
      &pid,
      &size,
      &processObject
    )

    guard status == noErr else { return nil }
    return processObject == kAudioObjectUnknown ? nil : processObject
  }

  /// Reads the stream format exposed by a tap object.
  static func streamFormat(of tapID: AudioObjectID) -> AudioStreamBasicDescription? {
    var address = AudioObjectPropertyAddress(
      mSelector: streamFormatSelector,
      mScope: kAudioObjectPropertyScopeGlobal,
      mElement: kAudioObjectPropertyElementMain
    )

    var format = AudioStreamBasicDescription()
    var size = UInt32(MemoryLayout<AudioStreamBasicDescription>.size)
    let status = AudioObjectGetPropertyData(
      tapID,
      &address,
      0,
      nil,
      &size,
      &format
    )
    return status == noErr ? format : nil
  }
}

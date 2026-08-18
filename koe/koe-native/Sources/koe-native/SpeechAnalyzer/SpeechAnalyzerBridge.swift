import AVFoundation
import Foundation
import Speech

/// Bridges on-device speech recognition to the Rust pipeline. Receives
/// canonical PCM audio chunks and emits transcription segments.
///
/// Recognizer: `SFSpeechRecognizer` with `requiresOnDeviceRecognition = true`.
/// The task file asks for `SFSpeechAnalyzer` (macOS 14+), but that class is
/// only available on macOS 15+ and does not exist in the macOS 14.4 SDK this
/// package builds against. `SFSpeechRecognizer` with on-device recognition
/// preserves the spec's core property (100% on-device, no network round-trip)
/// with the same streaming partial/final result contract.
///
/// Lifecycle: ``finalize()`` must be called before the bridge is released —
/// while running, the recognition task retains the bridge as its delegate
/// (and the bridge retains the task); the pair is only released at
/// ``finalize()`` (or when the task finishes on its own).
///
/// ``onSegment``/``onError`` are invoked on the recognizer's internal serial
/// queue; consumers that need a different threading model should dispatch as
/// needed (the Rust side forwards via a non-blocking tokio channel).
public final class SpeechAnalyzerBridge: NSObject, SFSpeechRecognitionTaskDelegate {
  /// Errors that can occur during recognition.
  public enum Error: Swift.Error {
    case unsupportedLocale
    case recognizerUnavailable
    case invalidAudio
    case recognitionFailed(String)
  }

  /// A single transcription segment produced by the recognizer.
  public struct Segment {
    /// The recognized text.
    public let text: String
    /// Start time offset in milliseconds from stream start.
    public let startMs: Int64
    /// End time offset in milliseconds from stream start.
    public let endMs: Int64
    /// Whether this segment is final or may still be updated.
    public let isFinal: Bool
    /// Confidence of the segment, 0.0–1.0.
    public let confidence: Float
  }

  /// Callback type for receiving transcription segments.
  public typealias SegmentCallback = (Segment) -> Void
  /// Callback type for receiving error messages.
  public typealias ErrorCallback = (Error) -> Void

  /// Serializes `feedAudio`/`finalize` calls: `SFSpeechRecognizer` requires
  /// audio buffers to be appended in order on a single queue.
  private let audioQueue = DispatchQueue(label: "koe.speech-analyzer.audio")

  /// Retains the recognizer for the lifetime of the bridge.
  private let recognizer: SFSpeechRecognizer
  /// The streaming request; kept alive so it can accept appended audio.
  private let request: SFSpeechAudioBufferRecognitionRequest
  /// The running recognition task; nil after finalization.
  private var task: SFSpeechRecognitionTask?

  /// Receives transcription segments (partial and final).
  public var onSegment: SegmentCallback?
  /// Receives error messages. Recognition errors are non-fatal — audio
  /// capture continues while transcription stops.
  public var onError: ErrorCallback?

  /// The locales supported by on-device recognition.
  public static var supportedLocales: [Locale] {
    SFSpeechRecognizer.supportedLocales().map { $0 as Locale }
  }

  /// Initializes the speech recognizer for the given locale.
  ///
  /// - Parameter locale: BCP-47 locale identifier (e.g., "en-US", "ja-JP").
  /// - Throws: ``Error/unsupportedLocale`` if the locale is not supported,
  ///   or ``Error/recognizerUnavailable`` if recognition is unavailable.
  public init(locale: String) throws {
    guard let recognizer = SFSpeechRecognizer(locale: Locale(identifier: locale)) else {
      throw Error.unsupportedLocale
    }
    guard recognizer.isAvailable else {
      throw Error.recognizerUnavailable
    }

    self.recognizer = recognizer

    let request = SFSpeechAudioBufferRecognitionRequest()
    request.requiresOnDeviceRecognition = true
    request.shouldReportPartialResults = true
    request.addsPunctuation = true
    self.request = request

    super.init()

    self.task = recognizer.recognitionTask(with: request, delegate: self)
  }

  deinit {
    request.endAudio()
    task?.cancel()
  }

  /// Feeds a chunk of PCM audio data to the recognizer.
  ///
  /// - Parameters:
  ///   - pcm: Interleaved Float32 PCM at 48 kHz stereo.
  ///   - frameCount: Number of frames (per channel) in the buffer.
  public func feedAudio(_ pcm: UnsafePointer<Float>, frameCount: Int) {
    guard frameCount > 0 else { return }
    audioQueue.async { [weak self] in
      guard let self else { return }
      guard let buffer = Self.makePCMBuffer(pcm: pcm, frameCount: frameCount) else {
        self.onError?(.invalidAudio)
        return
      }
      self.request.append(buffer)
    }
  }

  /// Signals end-of-stream and finalizes pending transcription results.
  ///
  /// Named `finalize` per the task file API; `NSObject.finalize` (overridden
  /// here) is a deprecated GC hook that is never invoked under ARC.
  public override func finalize() {
    audioQueue.async { [weak self] in
      guard let self else { return }
      self.request.endAudio()
      self.task?.finish()
      self.task = nil
    }
  }

  // MARK: - SFSpeechRecognitionTaskDelegate

  public func speechRecognitionTask(
    _ task: SFSpeechRecognitionTask,
    didHypothesizeTranscription transcription: SFTranscription
  ) {
    deliver(transcription.segments, isFinal: false)
  }

  public func speechRecognitionTask(
    _ task: SFSpeechRecognitionTask,
    didFinishRecognition recognitionResult: SFSpeechRecognitionResult
  ) {
    deliver(recognitionResult.bestTranscription.segments, isFinal: true)
  }

  public func speechRecognitionTaskWasCancelled(_ task: SFSpeechRecognitionTask) {
    onError?(.recognitionFailed("task was cancelled"))
  }

  public func speechRecognitionTask(
    _ task: SFSpeechRecognitionTask,
    didFinishSuccessfully successfully: Bool
  ) {
    if !successfully, let error = task.error {
      onError?(.recognitionFailed(error.localizedDescription))
    }
    self.task = nil
  }

  // MARK: - Segment mapping

  /// Maps transcription segments to ``Segment``s and forwards them.
  private func deliver(_ segments: [SFTranscriptionSegment], isFinal: Bool) {
    for source in segments {
      let startMs = Int64(source.timestamp * 1_000)
      let endMs = Int64((source.timestamp + source.duration) * 1_000)
      onSegment?(
        Segment(
          text: source.substring,
          startMs: startMs,
          endMs: endMs,
          isFinal: isFinal,
          confidence: source.confidence
        )
      )
    }
  }

  // MARK: - Audio conversion

  /// Builds a stereo `AVAudioPCMBuffer` from interleaved Float32 samples.
  private static func makePCMBuffer(
    pcm: UnsafePointer<Float>,
    frameCount: Int
  ) -> AVAudioPCMBuffer? {
    guard
      let format = AVAudioFormat(
        standardFormatWithSampleRate: 48_000,
        channels: 2
      )
    else { return nil }

    guard
      let buffer = AVAudioPCMBuffer(
        pcmFormat: format,
        frameCapacity: AVAudioFrameCount(frameCount)
      )
    else { return nil }
    buffer.frameLength = AVAudioFrameCount(frameCount)

    guard let channelData = buffer.floatChannelData else { return nil }
    // De-interleave: interleaved stereo Float32 → planar L/R channels.
    for frame in 0..<frameCount {
      channelData[0][frame] = pcm[frame * 2]
      channelData[1][frame] = pcm[frame * 2 + 1]
    }
    return buffer
  }
}

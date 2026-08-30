import KoeFfi
import XCTest

@testable import koe_native

final class KoeNativeTests: XCTestCase {
  func testKoeFfiAdd() {
    XCTAssertEqual(add(left: 2, right: 2), 4)
  }

  func testFfiCheckPermission() {
    let status = checkPermission(permission: .microphone)
    // No provider registered (permissions are pure Rust in the CLI); the FFI
    // read degrades to `notDetermined` rather than panicking.
    XCTAssertEqual(status, .notDetermined)
  }

  func testFfiEnumerateApps() {
    // The Rust discovery provider (installed by the CLI) owns app enumeration;
    // without it the FFI call degrades to an empty list rather than throwing.
    let apps = enumerateApps()
    XCTAssertTrue(apps.isEmpty)
  }

  func testSpeechAnalyzerInitialization() throws {
    do {
      let analyzer = try SpeechAnalyzerBridge(locale: "en-US")
      XCTAssertNotNil(analyzer)
      XCTAssertFalse(SpeechAnalyzerBridge.supportedLocales.isEmpty)
    } catch let error as SpeechAnalyzerBridge.Error {
      guard case .recognizerUnavailable = error else {
        return XCTFail("unexpected error \(error)")
      }
    }
  }

  func testCaptureErrorVariantsAreDistinguishable() {
    let errors: [CaptureError] = [
      .PermissionDenied("microphone"),
      .NoAudioSource(bundleId: "com.example.app"),
      .StreamError(msg: "underrun"),
      .Internal(msg: "boom"),
    ]

    XCTAssertEqual(errors.count, Set(errors.map { String(describing: $0) }).count)

    switch errors[0] {
    case .PermissionDenied(let msg):
      XCTAssertEqual(msg, "microphone")
    default:
      XCTFail("expected PermissionDenied")
    }
    switch errors[1] {
    case .NoAudioSource(let bundleId):
      XCTAssertEqual(bundleId, "com.example.app")
    default:
      XCTFail("expected NoAudioSource")
    }
    switch errors[2] {
    case .StreamError(let msg):
      XCTAssertEqual(msg, "underrun")
    default:
      XCTFail("expected StreamError")
    }
    switch errors[3] {
    case .Internal(let msg):
      XCTAssertEqual(msg, "boom")
    default:
      XCTFail("expected Internal")
    }
  }

  func testStartCaptureThrowsNoAudioSource() {
    XCTAssertThrowsError(
      try startCapture(
        source: .appAudio(bundleId: ""),
        callback: NoopAudioCallback()
      )
    ) { error in
      guard let captureError = error as? CaptureError,
        case .NoAudioSource = captureError
      else {
        return XCTFail("expected CaptureError.NoAudioSource, got \(error)")
      }
    }
  }

  func testStartTranscriptionThrowsUnsupportedLocale() {
    XCTAssertThrowsError(
      try startTranscription(locale: "", callback: NoopTranscriptionCallback())
    ) { error in
      guard let transcriptionError = error as? TranscriptionError,
        case .UnsupportedLocale = transcriptionError
      else {
        return XCTFail("expected TranscriptionError.UnsupportedLocale, got \(error)")
      }
    }
  }

  func testStartRecordingThrowsConfigError() {
    XCTAssertThrowsError(
      try startRecording(
        source: .microphone,
        outputPath: "",
        locale: "en-US",
        format: .ogg(bitrateBps: nil),
        enableAec: false,
        comfortNoise: false,
        progressCallback: NoopProgressCallback()
      )
    ) { error in
      guard let recordingError = error as? RecordingError,
        case .ConfigError = recordingError
      else {
        return XCTFail("expected RecordingError.ConfigError, got \(error)")
      }
    }
  }

  func testAudioCallbackReceivesPcmAndMonotonicTimestamps() throws {
    let callback = RecordingAudioCallback()
    let handle = try startCapture(source: .microphone, callback: callback)

    handle.deliverAudio(pcm: [0.1, -0.1], timestampMs: 10)
    handle.deliverAudio(pcm: [0.2, -0.2], timestampMs: 20)
    stopCapture(handle: handle)

    XCTAssertEqual(callback.calls.count, 2)
    XCTAssertEqual(callback.calls[0].pcm, [0.1, -0.1])
    XCTAssertEqual(callback.calls[0].timestampMs, 10)
    XCTAssertEqual(callback.calls[1].pcm, [0.2, -0.2])
    XCTAssertEqual(callback.calls[1].timestampMs, 20)
    XCTAssertLessThan(callback.calls[0].timestampMs, callback.calls[1].timestampMs)
  }

  func testAudioCallbackRetainedForHandleLifetime() throws {
    let callback = RecordingAudioCallback()
    let handle = try startCapture(source: .microphone, callback: callback)

    // Handle must retain the callback across the FFI boundary for the session.
    handle.deliverAudio(pcm: [0.5, -0.5], timestampMs: 5)
    XCTAssertEqual(callback.calls.count, 1)
    XCTAssertEqual(callback.calls[0].pcm, [0.5, -0.5])

    stopCapture(handle: handle)
  }

  func testProgressCallbackReceivesStatus() throws {
    let callback = RecordingProgressCallback()
    let handle = try startRecording(
      source: .microphone,
      outputPath: "/tmp/koe-test.ogg",
      locale: "en-US",
      format: .ogg(bitrateBps: nil),
      enableAec: false,
      comfortNoise: false,
      progressCallback: callback
    )

    handle.deliverStatus(
      status: RecordingStatus(
        elapsedMs: 42,
        bytesWritten: 100,
        levelLeft: 0.1,
        levelRight: 0.2,
        state: .recording
      )
    )
    _ = try stopRecording(handle: handle)

    XCTAssertEqual(callback.statuses.count, 1)
    XCTAssertEqual(callback.statuses[0].elapsedMs, 42)
    XCTAssertEqual(callback.statuses[0].state, .recording)
  }
}

private final class NoopAudioCallback: AudioCallback {
  func onAudio(pcm: [Float], timestampMs: UInt64) {}
}

private final class RecordingAudioCallback: AudioCallback {
  struct Call {
    let pcm: [Float]
    let timestampMs: UInt64
  }

  private(set) var calls: [Call] = []

  func onAudio(pcm: [Float], timestampMs: UInt64) {
    calls.append(Call(pcm: pcm, timestampMs: timestampMs))
  }
}

private final class NoopTranscriptionCallback: TranscriptionCallback {
  func onSegment(segment: TranscriptionSegment) {}
  func onError(error: String) {}
}

private final class NoopProgressCallback: ProgressCallback {
  func onStatus(status: RecordingStatus) {}
  func onSegment(segment: TranscriptionSegment) {}
  func onError(error: String) {}
}

private final class RecordingProgressCallback: ProgressCallback {
  private(set) var statuses: [RecordingStatus] = []

  func onStatus(status: RecordingStatus) {
    statuses.append(status)
  }

  func onSegment(segment: TranscriptionSegment) {}
  func onError(error: String) {}
}

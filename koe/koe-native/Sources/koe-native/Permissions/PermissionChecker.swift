import AVFoundation
import ApplicationServices
import Foundation

/// Checks and requests macOS TCC (Transparency, Consent, and Control)
/// permissions required by Koe.
public final class PermissionChecker: Sendable {
  /// Permission domains relevant to Koe.
  public enum Permission: Int32, CaseIterable {
    case microphone
    case screenRecording
    case accessibility
  }

  /// Current authorization status for a given permission.
  public enum Status: Int32, Equatable {
    case authorized
    case denied
    case restricted
    case notDetermined
  }

  /// Returns the current authorization status for the given permission.
  ///
  /// - Microphone: queries `AVCaptureDevice.authorizationStatus(for: .audio)`.
  /// - Screen Recording: uses `CGPreflightScreenCaptureAccess()` (lightweight;
  ///   cannot distinguish `.denied` from `.restricted` — both map to `.denied`).
  /// - Accessibility: uses `AXIsProcessTrusted()`.
  public static func status(for permission: Permission) -> Status {
    switch permission {
    case .microphone:
      switch AVCaptureDevice.authorizationStatus(for: .audio) {
      case .authorized: .authorized
      case .denied: .denied
      case .restricted: .restricted
      case .notDetermined: .notDetermined
      @unknown default: .notDetermined
      }
    case .screenRecording:
      // CGPreflightScreenCaptureAccess() returns a Bool; `restricted` is
      // indistinguishable from `denied` through this API.
      if CGPreflightScreenCaptureAccess() { .authorized } else { .denied }
    case .accessibility:
      if AXIsProcessTrusted() { .authorized } else { .denied }
    }
  }

  /// Requests authorization for the given permission.
  ///
  /// Only **microphone** triggers a system TCC dialog. Screen recording and
  /// accessibility cannot be requested programmatically for non-sandboxed
  /// apps — these return the current status without prompting.
  public static func request(_ permission: Permission) async -> Status {
    switch permission {
    case .microphone:
      await withCheckedContinuation { continuation in
        AVCaptureDevice.requestAccess(for: .audio) { granted in
          continuation.resume(returning: granted ? .authorized : .denied)
        }
      }
    case .screenRecording, .accessibility:
      status(for: permission)
    }
  }

  /// Returns the status of all permissions at once (batch check).
  /// Useful for CLI `koe permissions` and GUI onboarding screens.
  public static func allStatuses() -> [(Permission, Status)] {
    Permission.allCases.map { ($0, status(for: $0)) }
  }

  /// Opens System Settings to the privacy pane for the given permission.
  ///
  /// Uses `open(1)` with the `x-apple.systempreferences:` URL scheme so it
  /// works from both CLI and GUI contexts (no AppKit dependency).
  public static func openSystemSettings(for permission: Permission) {
    let pane =
      switch permission {
      case .microphone:
        "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone"
      case .screenRecording:
        "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture"
      case .accessibility:
        "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility"
      }
    let process = Process()
    process.executableURL = URL(fileURLWithPath: "/usr/bin/open")
    process.arguments = [pane]
    try? process.run()
  }
}

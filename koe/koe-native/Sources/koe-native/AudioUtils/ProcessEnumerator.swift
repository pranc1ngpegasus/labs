import ApplicationServices
import CoreAudio
import Foundation
import ScreenCaptureKit

/// Enumerates running macOS applications, optionally filtering by active audio.
public final class ProcessEnumerator: Sendable {
  /// Information about a running application.
  public struct AppInfo {
    /// Process ID.
    public var pid: Int32
    /// Localized application name.
    public var name: String
    /// Bundle identifier (nil for processes without a bundle).
    public var bundleID: String?
    /// Whether the process has an active audio output stream.
    public var hasAudio: Bool
  }

  // MARK: - Audio process enumeration

  /// Returns processes that currently have an audio object registered with
  /// CoreAudio, along with their display name and bundle identifier.
  ///
  /// Uses `NSWorkspace.runningApplications` for the app list and
  /// `kAudioHardwarePropertyTranslatePIDToProcessObject` to detect audio.
  public static func enumerateAudioProcesses()
    -> [(pid: Int32, name: String, bundleID: String?)]
  {
    let apps = NSWorkspace.shared.runningApplications
      .filter { $0.activationPolicy == .regular }
      .filter { $0.processIdentifier > 0 }

    return apps.compactMap { app in
      let pid = app.processIdentifier
      guard hasAudioObject(pid: pid) else { return nil }
      guard let name = app.localizedName else { return nil }
      return (pid: pid, name: name, bundleID: app.bundleIdentifier)
    }
  }

  // MARK: - ScreenCaptureKit

  /// Returns `SCShareableContent` for use with ScreenCaptureKit-based capture.
  public static func enumerateShareableContent() async throws
    -> SCShareableContent
  {
    try await SCShareableContent.current
  }

  // MARK: - Convenience

  /// Returns all regular-application processes with basic metadata.
  public static func enumerateApps() -> [AppInfo] {
    let audioPIDs = Set(enumerateAudioProcesses().map(\.pid))

    return NSWorkspace.shared.runningApplications
      .filter { $0.activationPolicy == .regular }
      .map { app in
        AppInfo(
          pid: app.processIdentifier,
          name: app.localizedName ?? "",
          bundleID: app.bundleIdentifier,
          hasAudio: audioPIDs.contains(app.processIdentifier)
        )
      }
  }

  /// Returns only applications that currently have active audio.
  public static func enumerateAudioApps() -> [AppInfo] {
    enumerateApps().filter(\.hasAudio)
  }

  // MARK: - Private helpers

  /// Queries CoreAudio to determine whether `pid` has an associated audio
  /// process object.
  private static func hasAudioObject(pid: Int32) -> Bool {
    var pid = pid  // mutable copy for inout
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

    return status == noErr && processObject != kAudioObjectUnknown
  }
}

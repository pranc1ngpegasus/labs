import Foundation
import KoeFfi

/// Bridges `koe-native` macOS APIs into the Rust FFI surface.
public final class KoeNativeProvider: NSObject, NativeProvider, @unchecked Sendable {
  public override init() {
    super.init()
  }

  public func checkPermission(permission: Permission) -> PermissionStatus {
    Self.mapStatus(PermissionChecker.status(for: Self.mapPermission(permission)))
  }

  public func requestPermission(permission: Permission) -> PermissionStatus {
    let mapped = Self.mapPermission(permission)
    let semaphore = DispatchSemaphore(value: 0)
    var result = PermissionStatus.notDetermined

    Task {
      let status = await PermissionChecker.request(mapped)
      result = Self.mapStatus(status)
      semaphore.signal()
    }

    semaphore.wait()
    return result
  }

  public func enumerateApps() -> [AppInfo] {
    ProcessEnumerator.enumerateApps().map { app in
      AppInfo(
        pid: app.pid,
        name: app.name,
        bundleId: app.bundleID,
        hasAudio: app.hasAudio
      )
    }
  }

  private static func mapPermission(_ permission: Permission) -> PermissionChecker.Permission {
    switch permission {
    case .microphone:
      .microphone
    case .screenRecording:
      .screenRecording
    case .accessibility:
      .accessibility
    }
  }

  private static func mapStatus(_ status: PermissionChecker.Status) -> PermissionStatus {
    switch status {
    case .authorized:
      .authorized
    case .denied:
      .denied
    case .restricted:
      .restricted
    case .notDetermined:
      .notDetermined
    }
  }
}

/// Installs the default `koe-native` provider for Rust FFI calls.
public enum KoeFfiBootstrap {
  private static let installed = {
    registerNativeProvider(provider: KoeNativeProvider())
  }()

  public static func install() {
    _ = installed
  }
}

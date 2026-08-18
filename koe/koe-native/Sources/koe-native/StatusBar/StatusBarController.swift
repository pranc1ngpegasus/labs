import AppKit
import Foundation

/// Manages a macOS menu bar item for Koe, providing quick access to
/// recording controls and status information when the GUI is closed.
///
/// Used internally by the GUI binary; not part of the C ABI surface
/// exposed to Rust.
public final class StatusBarController: Sendable {
  /// The current recording state displayed in the menu bar.
  public enum RecordingState {
    case idle
    case recording
    case paused
  }

  /// Creates a status bar controller.
  public init() {}

  /// Shows the status bar item.
  public func show() {}

  /// Hides the status bar item.
  public func hide() {}

  /// Updates the displayed recording state.
  public func updateState(_ state: RecordingState) {}

  /// Updates the duration label (e.g., "00:12:34").
  public func updateDuration(_ durationString: String) {}
}

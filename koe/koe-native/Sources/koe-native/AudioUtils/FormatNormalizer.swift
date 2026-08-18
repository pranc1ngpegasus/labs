import AudioToolbox
import CoreAudio
import Foundation

/// Converts audio from a source format to the canonical Koe format
/// (48 kHz, stereo, Float32, interleaved) using `AudioConverter`.
///
/// The converter is reusable for the lifetime of a capture session.
/// Primarily used for Process Tap sources which deliver audio in the
/// source process's native format.  SCK can be configured to deliver
/// canonical format directly, bypassing this normalizer.
///
/// Internal helper — not part of the public C ABI surface.
final class FormatNormalizer {
  /// Errors that can occur during normalizer lifecycle.
  enum Error: Swift.Error {
    case converterCreationFailed(OSStatus)
  }

  /// Canonical target format.
  static let canonicalASBD: AudioStreamBasicDescription = {
    var asbd = AudioStreamBasicDescription()
    asbd.mSampleRate = 48_000
    asbd.mFormatID = kAudioFormatLinearPCM
    asbd.mFormatFlags =
      kAudioFormatFlagIsFloat | kAudioFormatFlagIsPacked
    asbd.mBitsPerChannel = 32
    asbd.mChannelsPerFrame = 2
    asbd.mFramesPerPacket = 1
    asbd.mBytesPerFrame = 8  // 2 ch × 4 bytes
    asbd.mBytesPerPacket = 8
    return asbd
  }()

  /// Source format provided at init time.
  let sourceASBD: AudioStreamBasicDescription

  /// Reusable audio converter (lifetime = capture session).
  private let converter: AudioConverterRef

  /// Pre-computed maximum output packet size (in bytes per packet).
  private let maxOutputPacketSize: UInt32

  // MARK: - Init

  /// Creates a normalizer for the given source format.
  init(sourceDesc: AudioStreamBasicDescription) throws {
    sourceASBD = sourceDesc

    var source = sourceDesc
    var target = FormatNormalizer.canonicalASBD
    var converterRef: AudioConverterRef?
    let status = AudioConverterNew(&source, &target, &converterRef)
    guard status == noErr, let converterRef else {
      throw Error.converterCreationFailed(status)
    }
    converter = converterRef

    // Pre-compute maximum output size for buffer allocation.
    var maxSize: UInt32 = 0
    var propSize = UInt32(MemoryLayout<UInt32>.size)
    AudioConverterGetProperty(
      converter,
      kAudioConverterPropertyMaximumOutputPacketSize,
      &propSize,
      &maxSize
    )
    maxOutputPacketSize = maxSize
  }

  deinit {
    AudioConverterDispose(converter)
  }

  // MARK: - Conversion

  /// Converts PCM frames to the canonical format.
  ///
  /// - Parameters:
  ///   - input: Source PCM samples (interleaved if multi-channel).
  ///   - frameCount: Number of input frames.
  ///   - output: Pre-allocated output buffer (capacity ≥ `maxOutputFrames`).
  ///   - maxOutputFrames: Capacity of the output buffer in frames.
  /// - Returns: Number of output frames actually written, or 0 on error.
  func convert(
    input: UnsafePointer<Float>,
    frameCount: Int,
    output: UnsafeMutablePointer<Float>,
    maxOutputFrames: Int
  ) -> Int {
    let inputByteSize = UInt32(frameCount) * sourceASBD.mBytesPerFrame
    var outputPacketCount = UInt32(maxOutputFrames)

    let status = input.withMemoryRebound(
      to: UInt8.self, capacity: Int(inputByteSize)
    ) { inputBytes in
      output.withMemoryRebound(
        to: UInt8.self, capacity: maxOutputFrames * Int(targetASBD.mBytesPerFrame)
      ) { outputBytes in
        AudioConverterConvertBuffer(
          converter,
          inputByteSize,
          inputBytes,
          &outputPacketCount,
          outputBytes
        )
      }
    }

    guard status == noErr else { return 0 }
    return Int(outputPacketCount)
  }

  /// Convenience accessor for the target format.
  private var targetASBD: AudioStreamBasicDescription {
    FormatNormalizer.canonicalASBD
  }
}

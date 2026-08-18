import Darwin

/// Lock-free single-producer-single-consumer ring buffer for PCM audio.
///
/// The producer (native audio callback, real-time thread) writes samples via
/// ``write(_:count:)``. The consumer (AEC worker, tokio blocking thread) reads
/// samples via ``read(into:maxCount:)``.
///
/// Thread safety is ensured through `OSMemoryBarrier()` — each side only
/// writes its own index and reads the other side's index with a full memory
/// barrier, guaranteeing correct cross-thread visibility on Apple Silicon.
///
/// Recommended capacity: 4 × 20 ms chunks at 48 kHz stereo Float32 = 7680
/// samples.
final class RingBuffer {
  /// Total sample capacity.
  private let capacity: Int

  /// Underlying storage (unowned; lifetime equals RingBuffer lifetime).
  private let buffer: UnsafeMutablePointer<Float>

  /// Consumer position — only written by consumer.
  private var readIndex: Int = 0

  /// Producer position — only written by producer.
  private var writeIndex: Int = 0

  /// Creates a ring buffer with the given sample capacity.
  init(capacity: Int) {
    self.capacity = capacity
    buffer = UnsafeMutablePointer<Float>.allocate(capacity: capacity)
    buffer.initialize(repeating: 0, count: capacity)
  }

  deinit {
    buffer.deinitialize(count: capacity)
    buffer.deallocate()
  }

  /// Writes samples from `source` into the buffer.
  ///
  /// - Parameters:
  ///   - source: Interleaved Float32 PCM samples.
  ///   - count: Number of samples to attempt to write.
  /// - Returns: Number of samples actually written (may be less than `count`
  ///   if the buffer is full).
  func write(
    _ source: UnsafePointer<Float>,
    count: Int
  ) -> Int {
    // ── acquire consumer's latest position ──
    OSMemoryBarrier()
    let read = readIndex

    let used = writeIndex - read
    let space = capacity - used
    let toWrite = min(count, space)
    guard toWrite > 0 else { return 0 }

    let start = writeIndex % capacity
    let firstPart = min(toWrite, capacity - start)

    buffer.advanced(by: start).update(from: source, count: firstPart)
    if firstPart < toWrite {
      buffer.update(
        from: source.advanced(by: firstPart),
        count: toWrite - firstPart
      )
    }

    // ── publish new data to consumer ──
    OSMemoryBarrier()
    writeIndex &+= toWrite

    return toWrite
  }

  /// Reads samples from the buffer into `destination`.
  ///
  /// - Parameters:
  ///   - destination: Buffer to receive interleaved Float32 PCM samples.
  ///   - maxCount: Maximum number of samples to read.
  /// - Returns: Number of samples actually read (0 if the buffer is empty).
  func read(
    into destination: UnsafeMutablePointer<Float>,
    maxCount: Int
  ) -> Int {
    // ── acquire producer's latest position ──
    OSMemoryBarrier()
    let write = writeIndex

    let available = write - readIndex
    let toRead = min(maxCount, available)
    guard toRead > 0 else { return 0 }

    let start = readIndex % capacity
    let firstPart = min(toRead, capacity - start)

    destination.update(from: buffer.advanced(by: start), count: firstPart)
    if firstPart < toRead {
      destination.advanced(by: firstPart).update(
        from: buffer,
        count: toRead - firstPart
      )
    }

    // ── publish consumed space to producer ──
    OSMemoryBarrier()
    readIndex &+= toRead

    return toRead
  }

  /// The number of unread samples currently available.
  var availableSamples: Int {
    OSMemoryBarrier()
    return writeIndex - readIndex
  }
}

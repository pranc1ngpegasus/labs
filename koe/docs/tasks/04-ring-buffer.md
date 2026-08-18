---
title: 04 — Lock-Free Ring Buffer
status: draft
depends: [02-koe-native-package]
spec_refs: [10-recording-pipeline, 02-core-audio-process-tap]
---

# 04 — Lock-Free SPSC Ring Buffer

Implement the lock-free single-producer-single-consumer ring buffer in Swift.

## Location

`koe-native/Sources/AudioTap/RingBuffer.swift`

## Specification

| Parameter | Value |
|-----------|-------|
| Capacity | 7680 frames × 2 channels = 15,360 floats (~240 KB) |
| Sample format | Float32, interleaved stereo |
| Write behavior | Non-blocking; drops if full, increments drop counter |
| Read behavior | Non-blocking; returns 0 if empty |
| Thread safety | Atomic write/read indices (SPSC, lock-free) |
| Write cost | 1 `memcpy` + 2 atomic ops (target: ~1 µs on M-series) |

## Public API

```swift
public final class RingBuffer {
    public init(frameCapacity: Int, channelCount: Int = 2)
    public var availableRead: Int { get }
    public var availableWrite: Int { get }
    public var dropCount: UInt64 { get }

    /// Real-time audio callback. Never blocks. Returns false if full.
    @inline(__always)
    public func write(_ frames: UnsafePointer<Float>, count: Int) -> Bool

    /// Non-real-time consumer. Returns frames read, or 0 if empty.
    public func read(into buffer: UnsafeMutablePointer<Float>, maxFrames: Int) -> Int
}
```

## Real-Time Safety (Critical)

The `write` method is called from the Core Audio real-time thread. It MUST:
- **Not allocate memory** (no `malloc`, no Swift ARC allocation)
- **Not acquire locks** (no `os_unfair_lock`, no `pthread_mutex`)
- **Not perform I/O**
- **Not send Objective-C messages**

Use `UnsafeMutableBufferPointer<Float>` for storage, pre-allocated at init.
Use `OSAtomicInt64` or Swift atomics (`ManagedAtomic`) for indices.

## Implementation Notes

- Handle wrap-around correctly: if write position + count exceeds capacity, split into two `memcpy` calls
- `dropCount` is an atomic counter incremented on overflow

## Verification

- Unit test: write N frames, read back, verify data integrity
- Unit test: overflow behavior — write beyond capacity, verify drop count increments
- Unit test: wrap-around — fill to near-end, write across boundary, read back
- Performance test: measure write latency (must be < 1 µs on M1+)

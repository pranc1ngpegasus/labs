---
title: 23 — Audio Monitoring (Pass-Through)
status: draft
depends: [15-pipeline-core]
spec_refs: [10-recording-pipeline]
---

# 23 — Audio Monitoring

Route clean audio (post-AEC) to the default output device for live monitoring.

## Location

`koe-core/src/pipeline/monitor.rs`

## Signal Path

```
Ring Buffer → AEC → Clean Audio ──┬──→ Encoder
                                  └──→ AudioQueue Output (monitoring)
```

## Implementation

1. **Create `AudioQueue` output instance** at pipeline start if `--monitor` (CLI) or monitoring toggle (GUI) is enabled
2. **Configure for canonical format**: 48 kHz, Float32, 2ch interleaved
3. **Buffer**: 1 × 20ms (~960 frames) for minimal latency
4. **Feed**: Write clean audio block to AudioQueue, which plays to default output device
5. **Destroy**: Tear down AudioQueue at pipeline stop

## Latency Budget

| Component | Latency |
|-----------|---------|
| Ring buffer | ~0–20 ms (depending on fill) |
| AEC processing | ~5 ms |
| AudioQueue output buffer | ~10 ms (device buffer) |
| **Total** | **~15–35 ms** |

## User Controls

- CLI: `--monitor` / `-m` flag
- GUI: "Monitor" toggle in control bar

## Verification

- Enable monitoring, verify clean audio on output device
- Verify no feedback loop (AEC handles echo from speakers to mic)
- Verify monitoring stops cleanly on pipeline stop
- Measure end-to-end monitoring latency

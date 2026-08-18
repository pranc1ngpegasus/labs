---
title: 29 — CLI Signal Handling
status: draft
depends: [24-cli-record-command, 22-shutdown-sequence]
spec_refs: [08-cli-interface]
---

# 29 — Signal Handling

Implement signal handling for the CLI.

## Location

`koe-cli/src/signals.rs`

## Signal Behavior

| Signal | Behavior |
|--------|----------|
| SIGINT (1st press) | Graceful stop: finalize transcription, flush output, write summary. Exit code 5. |
| SIGINT (2nd press, within 2s) | Force stop; may lose partial transcript segments. Exit code 5. |
| SIGTERM | Same as SIGINT (1st press). |
| SIGUSR1 | Toggle pause/resume. |

## Implementation

```rust
use tokio::signal;

pub async fn handle_signals(
    pipeline: Arc<RecordingPipeline>,
    shutdown_flag: Arc<AtomicBool>,
) {
    let mut sigint = signal::signal(SignalKind::interrupt())?;
    let mut sigterm = signal::signal(SignalKind::terminate())?;
    let mut sigusr1 = signal::signal(SignalKind::from_raw(libc::SIGUSR1))?;

    loop {
        tokio::select! {
            _ = sigint.recv() => {
                if shutdown_flag.load(Ordering::Relaxed) {
                    // Second press → force exit
                    std::process::exit(5);
                }
                shutdown_flag.store(true, Ordering::Relaxed);
                // Spawn 2-second watchdog for double-tap
                let flag = shutdown_flag.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    flag.store(false, Ordering::Relaxed); // reset if we survived
                });
                break;
            }
            _ = sigterm.recv() => {
                shutdown_flag.store(true, Ordering::Relaxed);
                break;
            }
            _ = sigusr1.recv() => {
                if pipeline.is_paused() {
                    pipeline.resume();
                } else {
                    pipeline.pause();
                }
            }
        }
    }
}
```

## Implementation Notes

- Use `tokio::signal` for async-compatible signal handling
- `SIGUSR1` for pause/resume toggle (uncommon, not used by other tools)
- Double-tap detection via a 2-second timer that resets the flag

## Verification

- Start recording, press Ctrl-C → graceful stop, exit code 5
- Start recording, press Ctrl-C twice quickly → force exit
- Start recording, send SIGUSR1 → pause, send again → resume
- Send SIGTERM → same as Ctrl-C

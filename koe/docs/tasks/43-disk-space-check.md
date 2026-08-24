---
title: 43 — Disk Space Validation
status: draft
depends: [15-pipeline-core]
spec_refs: [11-data-formats]
---

# 43 — Pre-Recording Disk Space Check

Validate available disk space before starting a recording.

## Location

`koe-core/src/pipeline/disk_check.rs`

## Behavior

1. **Estimate output size** based on format and estimated duration:
   | Format | 1 hour estimate |
   |--------|----------------|
   | OGG Vorbis (q=0.4) | ~42 MB |

2. **Check available space** on output volume:
   ```rust
   fn check_disk_space(output_path: &Path, estimated_bytes: u64) -> Result<(), RecordingError> {
       let available = fs2::available_space(output_path)?;

       if available < estimated_bytes {
           return Err(RecordingError::InsufficientDiskSpace {
               needed: estimated_bytes,
               available,
           });
       }

       if available < estimated_bytes * 2 {
           // Warn but allow
           log::warn!(
               "Low disk space: {} available, {} estimated needed",
               format_bytes(available),
               format_bytes(estimated_bytes),
           );
       }

       Ok(())
   }
   ```

3. **Without a duration estimate** (no `--duration`):
   - Check that at least 100 MB is available (safety margin for streaming formats)
   - Warn if free space < 1 GB
   - Cannot guarantee success for long recordings without duration

## User-Facing Messages

### CLI
```
Warning: Only 500 MB free on output volume. Estimated need: 210 MB.
Proceed? [y/N]
```

```
Error: Insufficient disk space. Need 2.1 GB, have 500 MB.
Specify a different output directory or free up space.
```

### GUI
- Inline banner: "⚠ Low disk space (500 MB free)"
- Modal block: "Cannot start recording — need 2.1 GB, only 500 MB available"

## Verification

- Record to volume with ample space → no warning
- Record to volume with borderline space → warning
- Record to volume with insufficient space → error, recording refused
- Test with `--duration` → accurate size estimate
- Test without duration → fallback check

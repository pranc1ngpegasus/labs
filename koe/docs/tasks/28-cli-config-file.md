---
title: 28 — CLI Configuration File
status: draft
depends: [24-cli-record-command]
spec_refs: [08-cli-interface]
---

# 28 — Configuration File Loading

Implement TOML config file loading and merging with CLI args.

## Location

`koe-cli/src/config.rs`

## File Location

`~/.config/koe/config.toml` (XDG-compatible; override with `--config`)

## Schema

```toml
[defaults]
source = "system"
format = "ogg"
locale = "en-US"
transcript-format = "txt"
sample-rate = 48000
channels = 2

[aec]
enabled = true
comfort-noise = true

[output]
directory = "~/Recordings/Koe"

[transcription]
locale = "en-US"
transcript-format = "srt"
```

## Precedence

```
CLI flags (highest) > Config file > Built-in defaults (lowest)
```

## Implementation

1. **`KoeConfig` struct** (serde deserialized from TOML)
2. **`ConfigLoader`**
   - `load(default_path: Option<PathBuf>) -> Result<KoeConfig>`
   - Try default path, fall back to empty config
3. **Merge logic**
   - Each CLI `Option<T>` field overrides config when `is_some()`
   - Boolean flags: CLI `--no-aec` overrides config `enabled = true`
4. **Shell expansion**: expand `~` in paths using `dirs` crate

## Verification

- Create `~/.config/koe/config.toml` with custom defaults
- Run `koe record --source mic` → inherits other defaults from config
- Run `koe record --config /tmp/other.toml ...` → uses alternate config

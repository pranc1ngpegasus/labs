---
title: 25 — CLI `koe list` Command
status: draft
depends: [06-process-enumeration, 12-ffi-core-exports]
spec_refs: [08-cli-interface]
---

# 25 — CLI `koe list` Command

List capture-able apps and audio devices.

## Location

`koe-cli/src/commands/list.rs`

## Command Definition

```
koe list [OPTIONS]

Options:
  --audio-only         Only show apps with active audio
  --json               Output as JSON array
```

## Output (Text Mode)

```
  PID    NAME                  BUNDLE ID               HAS AUDIO
  ─────  ────────────────────  ──────────────────────  ─────────
  4201   Google Chrome         com.google.Chrome        yes
  8891   Spotify               com.spotify.client       yes
  1234   Finder                com.apple.Finder         no
```

## Output (JSON Mode)

```json
[
  {"pid": 4201, "name": "Google Chrome", "bundle_id": "com.google.Chrome", "has_audio": true},
  {"pid": 8891, "name": "Spotify", "bundle_id": "com.spotify.client", "has_audio": true}
]
```

## Implementation

1. Call `enumerate_apps()` via FFI (or `enumerateShareableContent()` for SCK)
2. If `--audio-only`, filter to `has_audio: true`
3. Format output as table or JSON
4. Print to stdout

## Verification

- `koe list` → prints all running apps with audio status
- `koe list --audio-only` → only apps currently playing audio
- `koe list --json | jq` → valid JSON

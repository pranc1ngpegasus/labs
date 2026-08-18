---
title: 27 — CLI `koe permissions` & `koe info`
status: draft
depends: [03-permission-checker, 12-ffi-core-exports]
spec_refs: [08-cli-interface]
---

# 27 — CLI Permissions Check & System Info

Implement `koe permissions` and `koe info` commands.

## Location

`koe-cli/src/commands/permissions.rs`
`koe-cli/src/commands/info.rs`

## `koe permissions`

```
koe permissions [OPTIONS]

Options:
  --json     Output as JSON
```

### Output (Text)

```
  PERMISSION          STATUS      FIX
  ─────────────────   ─────────   ──────────────────────────────────────
  Microphone          Authorized
  Screen Recording    Denied      Open System Settings → Screen Recording
  Accessibility       Denied      Open System Settings → Accessibility

If the terminal has permissions issues:
  Note: Permissions for "Terminal.app" differ from "Koe.app" (GUI).
  The GUI handles permission prompts automatically.
```

### Implementation
1. Call `check_permission()` for each of the three permissions
2. Format as table
3. For denied: provide actionable fix instructions with current macOS Settings path

## `koe info`

```
koe info [OPTIONS]

Options:
  --json     Output as JSON
```

### Output
- macOS version
- Default audio input device name and UID
- Default audio output device name and UID
- Supported locales for on-device speech recognition
- Available disk space on default output volume
- Koe version, build target, feature flags

### Implementation
1. Query `sw_vers` or `sysctl` for macOS version
2. Query Core Audio for default devices via FFI
3. Query `SFSpeechAnalyzer.supportedLocales()` via FFI
4. Query filesystem for disk space on default output directory
5. Print Koe version from `CARGO_PKG_VERSION` + feature flags

## Verification

- `koe permissions` → shows all three permissions
- `koe permissions --json | jq` → valid JSON
- `koe info` → prints system info
- `koe info --json | jq` → valid JSON

---
title: 01 — Workspace Setup
status: draft
depends: []
spec_refs: [01-architecture]
---

# 01 — Workspace Setup

Scaffold the Cargo workspace and build infrastructure.

## Crate Topology

```
koe-core/    — lib: pipeline, AEC, codecs, shared state (feature-gated)
koe-native/  — Swift package: macOS framework wrappers
koe-ffi/     — lib: uniffi-generated bindings, type conversions
koe-cli/     — bin: clap-driven CLI
koe-gui/     — bin: GPUI GUI (feature-gated, off by default)
```

## Tasks

1. **Workspace Cargo.toml**
   - Define `[workspace]` with all five members
   - Define workspace-level dependencies where shared (tokio, thiserror, log, etc.)
   - Feature flags: `aec` (on), `ogg` (on), `system-audio` (on), `screen-audio` (on), `cli` (on), `gui` (off)

2. **Crate scaffolding**
   - `koe-core/Cargo.toml` — lib crate; depends on `koe-ffi`
   - `koe-ffi/Cargo.toml` — lib crate; depends on `uniffi`
   - `koe-cli/Cargo.toml` — bin crate; depends on `koe-core`, `clap`, `tokio`
   - `koe-gui/Cargo.toml` — bin crate; depends on `koe-core`, `gpui` (git dep, pinned rev)

3. **Nix flake**
   - Shell with Rust toolchain (from `rust-toolchain.toml`), Swift, Xcode CLI tools
   - `rustPlatform` build for `koe-cli`
   - macOS-specific `darwin` inputs for frameworks

4. **Rust toolchain**
   - `rust-toolchain.toml` — pin stable channel (already exists)
   - `.cargo/config.toml` — set build target to `aarch64-apple-darwin` or `x86_64-apple-darwin`

5. **Linting/formatting**
   - `.clippy.toml` — project-specific lints (already exists)
   - `.rustfmt.toml` — formatting rules (already exists)
   - `.taplo.toml` — TOML formatting (already exists)
   - `.pre-commit-config.yaml` — pre-commit hooks (already exists)

## Verification

```bash
cargo check --workspace
cargo build --workspace
cargo test --workspace
```

# labs

A collection of personal open-source projects and experiments. It is a
Rust workspace + Nix flake monorepo that currently houses three projects:
**`koe`**, **`ren`**, and **`sui`**.

Each project is maintained as an independent git repository (with remotes
`mokmok-dev/koe`, `mokmok-dev/ren`, and `mokmok-dev/sui` respectively); this
repository builds, verifies, and ships them as a single workspace.

## Projects

### [koe](./koe) — 声 (voice)

An **offline recording & transcription tool** for macOS.

- Captures system audio via Core Audio Process Tap / ScreenCaptureKit
- Speech recognition uses Apple's on-device SpeechAnalyzer only — no audio ever leaves the machine
- Core (pipeline, AEC, codecs) is written in Rust; the native layer is a thin Swift shim over macOS frameworks with a uniffi FFI boundary
- Frontend is `koe-cli` (CLI); a GUI (GPUI) is in design

```console
koe record --source system --output meeting.ogg
koe list --audio-only
koe transcribe meeting.ogg --format srt
koe permissions --check
```

The design spec (`docs/spec/`) and implementation task breakdown (`docs/tasks/`)
live in [koe/docs](./koe/docs).

**Crate layout:** `koe-core` (pipeline, AEC, codecs, shared state) → `koe-ffi` (uniffi-generated bindings) → `koe-native` (Swift package) + `koe-cli` (CLI binary)

### [ren](./ren) — 蓮・連・錬 (lotus · connection · refinement)

A foundation for continuous development with coding agents. Provides
**deterministic Rhai workflows** (ren-workflow) and **durable local memory**
(ren-memory).

- Built around a 5-petal development process: make requirements less dumb → delete → simplify → accelerate cycle times → automate
- Workflows are Rhai scripts with declarative schemas. The echo run verifies the execution plan before you carry it out with real agents
- Memory accumulates knowledge in a local SQLite vault, fed back into later sessions via a Codex hook

```console
ren workflow list
ren workflow show <name>
ren workflow run <name-or-path> --args '<json>'
ren memory search <query>
ren init        # installs skills into supported agents
```

See [ren/README.md](./ren/README.md) for details.

### [sui](./sui) — 粋・推・遂 (essence · infer · accomplish)

A lightweight, robust **coding-agent TUI**.

- Closes the loop with OpenAI-compatible function calling (`tools` / `tool_calls`) instead of custom protocols or heavy wrappers
- Indexes the workspace with BM25 and helps with code work via a minimal toolset: `code_search` / `edit` / `bash`
- Includes a workflow engine (content-hash journal replay) plus research and design for session-spanning memory

```console
sui
```

Starting it opens a TUI over the indexed current directory. Research notes on
tool calling and the memory engine live in [docs/research/](./sui/docs/research).

**Crate layout:** `sui` (binary) / `sui-app` (TUI) / `sui-agent` (turn loop) / `sui-llm` (wire format) / `sui-tools` (tool execution & search) / `sui-theme` / `sui-widget` / `sui-workflow` (workflow engine)

## Repository Layout

| Path | Contents |
| --- | --- |
| `koe/` | Koe (声) — offline recording & transcription for macOS |
| `ren/` | Ren (蓮・連・錬) — workflow + memory foundation |
| `sui/` | Sui (粋・推・遂) — coding-agent TUI |
| `Cargo.toml` | Cargo workspace definition and shared dependencies |
| `flake.nix` | Nix build definition (devShell, packages, pre-commit, treefmt) |
| `.github/workflows/` | CI — runs `nix flake check` on PRs and pushes to `main` |

## Development

### Prerequisites

- [Nix](https://nixos.org/) (with flakes)
- [direnv](https://direnv.net/) (optional)

### Setup

```console
nix develop     # or: direnv allow
```

The Rust toolchain (latest stable via `rust-overlay`) is provided by the flake.
`Cargo.lock` is committed.

### Common Commands

| Purpose | Command |
| --- | --- |
| Build | `cargo build --workspace` |
| Test | `cargo test --workspace` |
| Lint | `cargo clippy --workspace --all-targets -- -D warnings` |
| Format | `cargo fmt --all` |
| Full Nix check (same as CI) | `nix flake check` |
| Build a package | `nix build .#koe` / `.#ren` / `.#sui` |

## Design Principles

- **First-principles thinking + a 5-step design process** (question the requirements → delete → simplify → accelerate cycle time → automate), codified as implementation discipline in each subproject's `AGENTS.md`
- **No "just in case"** — anything that can be added back later is not built up front
- **Strict workspace lints** (Rust 2024 edition; `unsafe_code`, `unwrap_used`, and `panic` denied; clippy `pedantic` + `nursery` denied)
- Each binary (`koe` / `ren` / `sui`) is packaged for Mac/Linux via Nix (crane) and exposed as `packages.<system>.*`
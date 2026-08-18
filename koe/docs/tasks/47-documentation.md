---
title: 47 — Documentation
status: draft
depends: [all]
spec_refs: [00-index]
---

# 47 — User & Developer Documentation

Write comprehensive documentation for users and developers.

## Location

`docs/` directory and `README.md`

## Documents to Create

### 1. README.md (Project Root)
- Koe overview and value proposition
- Screenshots / GIF (CLI and GUI)
- Quick-start instructions
- Installation (Homebrew, cargo install, download .app)
- Feature list
- Link to full docs

### 2. User Guide (`docs/user-guide.md`)
- Quick start (first recording in 60 seconds)
- CLI walkthrough with examples:
  - Recording Chrome audio
  - Recording a Zoom call with microphone + AEC
  - Transcribing an existing file
  - Changing output formats
- GUI walkthrough:
  - Permissions setup
  - Starting a recording
  - Reading the live transcript
  - Exporting
- Configuration file reference
- Troubleshooting / FAQ:
  - "Why can't I capture app X?"
  - "Audio input monitoring permission isn't working"
  - "Transcription language isn't available"
  - "Disk space errors"

### 3. Build Guide (`docs/build-guide.md`)
- Prerequisites (Xcode, Rust, Swift)
- Cloning and building
- Nix flake usage
- Feature flags and their effects
- Running in development

### 4. Architecture Reference (`docs/architecture.md`)
- Crate topology diagram
- Data flow diagram
- Threading model
- FFI boundary
- Link to spec docs for implementation details

### 5. Contributing Guide (`CONTRIBUTING.md`)
- Code of conduct
- Development setup
- Testing strategy
- PR process
- Commit conventions

### 6. Changelog (`CHANGELOG.md`)
- Initial v0.1.0 release notes

## Verification

- README renders correctly on GitHub
- All internal links work
- CLI examples are copy-pasteable and correct
- Screenshots are up-to-date

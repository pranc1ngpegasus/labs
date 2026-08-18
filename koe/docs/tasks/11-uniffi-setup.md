---
title: 11 — uniffi Configuration & Build
status: draft
depends: [01-workspace-setup, 02-koe-native-package]
spec_refs: [07-native-bridge]
---

# 11 — uniffi Configuration & Build Integration

Set up uniffi-rs for generating Swift↔Rust C ABI bindings.

## Location

`koe-ffi/` crate

## Tasks

1. **Add uniffi dependency**
   ```toml
   [dependencies]
   uniffi = { version = "0.28", features = ["cli"] }

   [build-dependencies]
   uniffi = { version = "0.28", features = ["build"] }
   ```

2. **Create `build.rs`**
   - Use `uniffi::generate_scaffolding` to generate Rust scaffolding from UDL or proc-macro exports
   - If using proc-macro approach, no UDL file needed; just `#[uniffi::export]` annotations

3. **uniffi-bindgen integration**
   - Generate Swift file (`koe_ffi.swift`) and C header (`koe_ffi.h`) via:
     ```bash
     uniffi-bindgen generate --language swift --out-dir generated/
     ```
   - Swift-side module map configuration for consuming C header

4. **Build pipeline**
   ```
   cargo build --release -p koe-ffi          → libkoe_ffi.a
   uniffi-bindgen generate ...                → koe_ffi.swift + koe_ffi.h
   swift build (consumes .a + .swift + .h)   → libkoe_native.dylib
   ```

5. **module.modulemap**
   - Create for koe-native to import the C header:
     ```
     module KoeFfi {
         header "koe_ffi.h"
         export *
     }
     ```

## Verification

```bash
cargo build -p koe-ffi
uniffi-bindgen generate --language swift ...
# Verify generated .swift compiles
# Verify generated .h is valid C
```

//! Builds the Speech framework wrapper exactly like shiguredo/audio-device-rs:
//! compile the Objective-C shim with `cc` on macOS, link the system frameworks,
//! and generate the FFI bindings from `speech.h` with `bindgen`. Non-macOS
//! targets skip all of it (the crate exposes no-op stubs there).

#![allow(clippy::expect_used, clippy::pedantic)]

use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo::rerun-if-changed=src/speech.h");
    println!("cargo::rerun-if-changed=src/speech.m");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }

    cc::Build::new()
        .file("src/speech.m")
        .flag("-fobjc-arc")
        .compile("koe_speech");

    println!("cargo::rustc-link-lib=framework=AVFoundation");
    println!("cargo::rustc-link-lib=framework=CoreFoundation");
    println!("cargo::rustc-link-lib=framework=Foundation");
    println!("cargo::rustc-link-lib=framework=Speech");

    let bindings = bindgen::Builder::default()
        .header("src/speech.h")
        .allowlist_function("koe_speech_.*")
        .allowlist_type("KoeSpeech.*")
        .allowlist_var("KOE_SPEECH_.*")
        .derive_debug(true)
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        .generate()
        .expect("failed to generate speech.h bindings");

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR must be set"));
    bindings
        .write_to_file(out_dir.join("speech_bindings.rs"))
        .expect("failed to write speech.h bindings");
}

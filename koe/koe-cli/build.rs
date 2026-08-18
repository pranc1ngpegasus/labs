//! Embeds an Info.plist into the `koe` binary so macOS frameworks (Speech in
//! particular) see a proper bundle identity and usage-description keys.
//!
//! `koe record` / `koe transcribe` talk to `SFSpeechRecognizer` directly from
//! Rust. When speech-recognition authorization is `NotDetermined`, the system
//! shows a prompt that reads `NSSpeechRecognitionUsageDescription` from the
//! process's Info.plist — and *crashes* if that key is missing. Unbundled CLI
//! tools have no Info.plist unless one is embedded via the dedicated ld64
//! `-sectcreate __TEXT __info_plist` section, which `NSBundle.mainBundle`
//! then discovers.

use std::env;
use std::fs;
use std::path::PathBuf;

const INFO_PLIST: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleIdentifier</key>
  <string>dev.mokmok.koe</string>
  <key>CFBundleName</key>
  <string>koe</string>
  <key>CFBundleExecutable</key>
  <string>koe</string>
  <key>CFBundleVersion</key>
  <string>1</string>
  <key>CFBundleShortVersionString</key>
  <string>0.0.0</string>
  <key>NSSpeechRecognitionUsageDescription</key>
  <string>Koe transcribes recorded audio using macOS speech recognition.</string>
</dict>
</plist>
"#;

fn main() {
    if !cfg!(target_os = "macos") {
        return;
    }
    println!("cargo:rerun-if-changed=build.rs");

    let Some(out_dir) = env::var_os("OUT_DIR").map(PathBuf::from) else {
        // Build scripts always receive OUT_DIR; skip embedding if missing.
        eprintln!("cargo:warning=OUT_DIR is unset; skipping Info.plist embedding");
        return;
    };
    let plist_path = out_dir.join("Info.plist");
    if let Err(err) = fs::write(&plist_path, INFO_PLIST) {
        println!("cargo:warning=failed to write embedded Info.plist: {err}");
        return;
    }

    // -Wl,-sectcreate,__TEXT,__info_plist,<path> on ld64; also supported by
    // lld's darwin driver. Applied to the `koe` binary only.
    let flag = format!(
        "-Wl,-sectcreate,__TEXT,__info_plist,{}",
        plist_path.display()
    );
    println!("cargo:rustc-link-arg-bins={flag}");
}

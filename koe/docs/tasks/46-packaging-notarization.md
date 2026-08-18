---
title: 46 — Packaging & Notarization
status: draft
depends: [31-gui-gpui-scaffold, 24-cli-record-command]
spec_refs: [06-permission-model, 09-gui-interface]
---

# 46 — .app Bundle, Code Signing & Notarization

Package the GUI app and CLI binary for distribution.

## Location

`scripts/` (packaging scripts), `koe-gui/Info.plist`, `koe-gui/koe.entitlements`

## .app Bundle Structure

```
Koe.app/
  Contents/
    Info.plist
    MacOS/
      Koe                     ← koe-gui binary
    Resources/
      AppIcon.icns
      PrivacyInfo.xcprivacy
    Frameworks/
      libkoe_native.dylib     ← koe-native .dylib
      libkoe_ffi.a            ← (static, linked into binary)
```

## Info.plist

```xml
<key>CFBundleName</key>
<string>Koe</string>
<key>CFBundleIdentifier</key>
<string>dev.mokmok.koe</string>
<key>CFBundleVersion</key>
<string>1</string>
<key>CFBundleShortVersionString</key>
<string>0.1.0</string>
<key>LSMinimumSystemVersion</key>
<string>14.0</string>
<key>NSMicrophoneUsageDescription</key>
<string>Koe needs microphone access to record your voice during calls and meetings. Audio is processed entirely on-device and is never uploaded.</string>
<key>NSScreenCaptureUsageDescription</key>
<string>Koe needs screen recording access to capture audio from apps during recording. Screen video is discarded immediately — only audio is kept.</string>
```

## Entitlements

```xml
<key>com.apple.security.device.audio-input</key><true/>
<key>com.apple.security.device.screen-recording</key><true/>
<key>com.apple.security.cs.disable-library-validation</key><true/>
<key>com.apple.security.get-task-allow</key><true/>
<!-- NOT sandboxed — required for Process Tap -->
<key>com.apple.security.app-sandbox</key><false/>
```

## Build Script

```bash
#!/bin/bash
# scripts/package.sh

set -euo pipefail

PROFILE="${1:-release}"
APP_DIR="Koe.app"

# 1. Build Rust binary
cargo build --profile "$PROFILE" -p koe-gui --features gui

# 2. Build koe-native .dylib
cd koe-native && swift build -c release && cd ..

# 3. Create .app bundle
mkdir -p "$APP_DIR/Contents/MacOS"
mkdir -p "$APP_DIR/Contents/Resources"
mkdir -p "$APP_DIR/Contents/Frameworks"

cp "target/$PROFILE/koe-gui" "$APP_DIR/Contents/MacOS/Koe"
cp "koe-gui/Info.plist" "$APP_DIR/Contents/"
cp "koe-gui/Assets.xcassets/AppIcon.icns" "$APP_DIR/Contents/Resources/"
cp "koe-gui/PrivacyInfo.xcprivacy" "$APP_DIR/Contents/Resources/"
cp "koe-native/.build/arm64-apple-macosx/release/libkoe_native.dylib" "$APP_DIR/Contents/Frameworks/"

# 4. Fix library paths
install_name_tool -change "@rpath/libkoe_native.dylib" \
    "@executable_path/../Frameworks/libkoe_native.dylib" \
    "$APP_DIR/Contents/MacOS/Koe"

# 5. Code sign
codesign --deep --force --verify --verbose \
    --sign "Developer ID Application: MokMok Dev (XXXXXXXXXX)" \
    --entitlements "koe-gui/koe.entitlements" \
    --options runtime \
    "$APP_DIR"

# 6. Notarize
xcrun notarytool submit "$APP_DIR.zip" \
    --apple-id "dev@mokmok.dev" \
    --team-id "XXXXXXXXXX" \
    --password "@keychain:AC_PASSWORD" \
    --wait

# 7. Staple ticket
xcrun stapler staple "$APP_DIR"
```

## CLI Distribution

```bash
# For CLI-only distribution:
cargo build --release -p koe-cli
# Distribute as single binary or via Homebrew formula
```

## Requirements for Notarization

1. **Privacy policy** bundled as `PrivacyInfo.xcprivacy`
2. **Hardened Runtime** (`--options runtime` in codesign)
3. **Every entitlement justified** in App Review notes
4. **`com.apple.security.cs.disable-library-validation`** needs specific technical justification referencing Core Audio Process Tap
5. Binary must pass `codesign --verify --deep --strict` and Gatekeeper checks

## Verification

```bash
# Verify bundle structure
ls -R Koe.app/Contents/

# Verify code signing
codesign --verify --deep --strict --verbose Koe.app

# Verify entitlements
codesign -d --entitlements :- Koe.app

# Verify notarization
spctl --assess --verbose Koe.app
```

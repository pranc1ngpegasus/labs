---
title: Nix パッケージング — flake 統合・システム依存・libopus prebuilt
topic: nix-packaging
status: draft
date: 2026-08-24
depends: [02-architecture]
---

# 05 — Nix パッケージング

## 前提: リポジトリの既存ビルド構造

`labs` は Cargo workspace 全体を **crane の単一 derivation** でビルドし、各プロジェクトの
`package.nix` は生成された `bin/<name>` を専用出力にコピーする構成(koe/sui/ren で実績)。

```
flake.nix
├─ commonArgs      : src(フィルタ済み), pname=labs-workspace, version=0.0.0, strictDeps
├─ cargoArtifacts  : craneLib.buildDepsOnly commonArgs   ← 依存のビルド結果(全プロジェクト共有)
├─ checks          : fmt / clippy(--all-targets --all-features, -D warnings) / hakari / test
└─ packages        : { codex-proxy, sui, koe, ren, default = symlinkJoin … }
systems            : x86_64-linux, aarch64-linux, aarch64-darwin
CI                 : ubuntu-latest で `nix flake check`(x86_64-linux のみ)
```

oto も同じパターンに載せる。追加が必要なのは「oto 固有のシステム依存」と
「shiguredo_opus のオフライン化」の 2 点。

## oto 固有のシステム依存

| system | ビルド時 | 実行時 |
|---|---|---|
| Linux | `pkg-config`、`libpulse.dev`(PulseAudio ヘッダ + `.pc`)、`libclang`(bindgen 用)、`tar`(stdenv) | `libpulse.so.0`(動的リンク) |
| macOS | `libclang`(bindgen 用)。CoreAudio / AudioToolbox / Foundation はシステム SDK | なし(フレームワークは OS 標準) |
| Windows | なし(WASAPI は `windows` crate、behind feature) | なし |

ポイント:

- `shiguredo_audio_device` の build.rs は **bindgen** を実行し、macOS では
  `-framework AudioToolbox/CoreAudio/Foundation`、Linux では `pkg-config --libs libpulse` を
  emit する。したがって bindgen 用 **libclang**(`LIBCLANG_PATH` env)が Linux/macOS 両方で必要。
- `shiguredo_opus` の build.rs は **prebuilt 経路では bindgen を実行しない**(bindings.rs を
  tarball からコピーするだけ)ため、あちらに libclang は不要。
- Linux 実行バイナリは `libpulse.so.0` に動的リンクする。crane のビルド結果には rpath が
  付かないため、パッケージング側で `autoPatchelfHook` により rpath を補う。

## shiguredo_opus のビルド問題と対処

### 問題

`shiguredo_opus`(2026.2.0)の build.rs は、ビルド時に **curl で GitHub Releases の prebuilt
`libopus-<target>.tar.gz` をネットワーク取得**する(または `source-build` feature で
oss-mirrors.shiguredo.jp からソースを取得して CMake ビルド)。Nix サンドボックスはネットワークが
遮断されるため、そのままでは `buildDepsOnly` / `buildPackage` が失敗する。

検討した選択肢:

| 案 | 評価 |
|---|---|
| (1) prebuilt を `pkgs.fetchurl`(FOD)で取得し、build.rs に**環境変数による注入パッチ**を当てる | **採用**。sha256 は FOD が担保、パッチは最小 |
| (2) `source-build` feature + cmake でソースからビルド | llvm-nm / llvm-objcopy(rustup llvm-tools)を要求。rust-overlay の toolchain には無く、sysroot パス探索も nix 環境と噛み合わない |
| (3) `[patch.crates-io]` でフォーク(git dependency)に差し替え | フォーク管理が必要。パッチ量が同じなら (1) がシンプル |
| (4) prebuilt をリポジトリに vendoring | 2 MB × 3 system のバイナリをコミットするのは却下 |

### 採用案: 環境変数注入パッチ

**パッチ**: `oto/nix/patches/shiguredo_opus-prebuilt.patch`
(crates.io ソースの `build.rs` に対する **crate ルート相対** diff。crane の
`overrideVendorCargoPackage` で vendor 済みソースに直接適用する)

```diff
 fn download_prebuilt(out_dir: &Path) -> PathBuf {
     let target = get_target_platform();
     let version = env::var("CARGO_PKG_VERSION").expect("CARGO_PKG_VERSION is not set");
     let base_url = format!(
         "https://github.com/shiguredo/opus-rs/releases/download/{}",
         version
     );
     let archive_name = format!("libopus-{}.tar.gz", target);
-    let archive_url = format!("{}/{}", base_url, archive_name);
-    let sha256_url = format!("{}/{}.sha256", base_url, archive_name);
 
     let archive_path = out_dir.join("prebuilt.tar.gz");
-    let sha256_path = out_dir.join("prebuilt.sha256");
     let prebuilt_dir = out_dir.join("prebuilt");
     fs::create_dir_all(&prebuilt_dir).expect("failed to create prebuilt directory");
 
-    // curl でアーカイブをダウンロード
-    eprintln!("prebuilt ライブラリをダウンロード中: {}", archive_url);
-    let status = Command::new("curl")
-        .args(["-fsSL", "-o"])
-        .arg(&archive_path)
-        .arg(&archive_url)
-        .status()
-        .expect("failed to execute curl. Ensure curl is installed");
-    if !status.success() {
-        panic!("failed to download prebuilt library: {}", archive_url);
-    }
-
-    // curl で SHA256 チェックサムをダウンロード
-    let status = Command::new("curl")
-        .args(["-fsSL", "-o"])
-        .arg(&sha256_path)
-        .arg(&sha256_url)
-        .status()
-        .expect("failed to execute curl");
-    if !status.success() {
-        panic!("failed to download SHA256 checksum: {}", sha256_url);
-    }
-
-    // SHA256 を検証
-    verify_sha256(&archive_path, &sha256_path);
+    // Nix(オフラインサンドボックス)向け: 取得済み tarball を環境変数から注入する。
+    // sha256 は Nix 側の fetchurl(FOD)で担保済み。
+    if let Ok(tarball) = env::var("OPUS_PREBUILT_TARBALL") {
+        fs::copy(&tarball, &archive_path).expect("failed to copy prebuilt tarball");
+    } else {
+        // 通常経路(ネットワークあり): 従来どおり curl で取得し sha256 検証
+        let archive_url = format!("{}/{}", base_url, archive_name);
+        let sha256_url = format!("{}/{}.sha256", base_url, archive_name);
+        /* … 従来コード(curl + verify_sha256)はそのまま … */
+    }
 
     // tar で展開(以降は共通)
```

- パッチを当てても **ローカル開発(ネットワークあり)の plain `cargo build` は従来どおり動く**。
- `get_target_platform()` は Linux 上で `/etc/os-release` を参照し、非 Ubuntu では panic するため、
  `OPUS_TARGET` env を併せて設定する(NixOS でも panic しない。値は FOD と整合させる)。

### flake.nix への組み込み(perSystem 内)

```nix
let
  # ターゲット別の prebuilt アセット(FOD)
  opusTargets = {
    x86_64-linux   = { asset = "ubuntu-24.04_x86_64"; sha256 = "c028f0…"; };
    aarch64-linux  = { asset = "ubuntu-24.04_arm64";  sha256 = "129f4d…"; };
    aarch64-darwin = { asset = "macos_arm64";         sha256 = "ebb588…"; };
  };
  opusPrebuilt = pkgs.fetchurl {
    url = "https://github.com/shiguredo/opus-rs/releases/download/2026.2.0/libopus-${opusTargets.${system}.asset}.tar.gz";
    sha256 = opusTargets.${system}.sha256;
  };
  audioNativeBuildInputs = [ pkgs.pkg-config pkgs.llvmPackages.libclang ];
  audioBuildInputs = pkgs.lib.optionals pkgs.stdenv.isLinux [ pkgs.libpulse.dev ];
  audioEnv = {
    LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
    OPUS_TARGET = "ubuntu_24.04";      # get_target_platform の panic 回避(値自体は未使用)
    OPUS_PREBUILT_TARBALL = "${opusPrebuilt}";
  };
  # crane の cargoPatches は transitive(vendored)な依存クレートには効かないため、
  # overrideVendorCargoPackage で vendor ツリー内の旧ソースに直接 patch を当てる。
  # crane は .cargo-checksum.json を {"files":{}} で書くため checksum 再検証は起きない。
  overrideVendorCargoPackage = p: drv: if p.name == "shiguredo_opus" then
    pkgs.runCommandLocal "shiguredo_opus-patched" { } ''
      cp -a ${drv} $out
      chmod -R u+w $out
      patch -d $out -p1 < ${./oto/nix/patches/shiguredo_opus-prebuilt.patch}
    ''
  else drv;
  # overrideVendorCargoPackage は関数なので mkDerivation の env に漏れてはいけない。
  # cargoVendorDir として組み立ててから引き渡す。
  cargoVendorDir = craneLib.vendorCargoDeps { inherit src overrideVendorCargoPackage; };
in
# commonArgs に追加: cargoVendorDir / nativeBuildInputs / buildInputs / env は
# buildDepsOnly と buildPackage の両方(buildDepsOnly で build.rs が走るため)に必要
```

FOD の sha256(2026.2.0 リリースアセットから検証済み):

| system | アセット | sha256 |
|---|---|---|
| `aarch64-darwin` | `libopus-macos_arm64.tar.gz` | `ebb588a606c050744cc673159df77b256686650506f3f7eb5111dbf40a6000ad` |
| `x86_64-linux` | `libopus-ubuntu-24.04_x86_64.tar.gz` | `c028f032718147b82c3ba4a4148548c95106717f081f32ad14a2de3b864e6f8f` |
| `aarch64-linux` | `libopus-ubuntu-24.04_arm64.tar.gz` | `129f4d6ccbb8598f97d8ba77fab5db431ead7151f3b86a6253fc55a4c441c449` |

> Ubuntu 24.04 を基準にする。Ubuntu 22.04 のほうが glibc 互換が広いが、nixpkgs-unstable の
> glibc(Newer)に対する前方互換は 24.04 で問題ない。リンク問題が出た場合は 22.04 へ切替可能。

補足:

- **checks(hakari / clippy / test)にも `audioBuildInputs` / `audioEnv` を展開する**。
  特に `checks.test`(workspace `cargo test`)は oto のテストバイナリが `libpulse.so.0` に
  リンクするため `buildInputs` が必須(実行時探索のため `LD_LIBRARY_PATH` も必要なら env で明示)。
- **`overrideVendorCargoPackage` の注意**: crane の `cargoPatches` は transitive(registry)な
  vendor ソースへは適用されない(検証済み)。`overrideVendorCargoPackage` は `downloadCargoPackage`
  の出力(= unpack 済み crate、`.cargo-checksum.json` は `{"files":{}}`)を加工するため、
  ファイル改変後の checksum 再検証は発生しない。パッチのパスは **crate ルート相対**
  (`a/build.rs` / `patch -p1`)で書く。

### Cargo.toml 側のバージョン固定

依存の置き場所はクレート分割(02 参照)に合わせる:

- `shiguredo_opus` → `oto-encode/Cargo.toml`(DRED 不要なので `default-features = false`)
- `shiguredo_audio_device` → `oto-capture/Cargo.toml`

```toml
# oto-encode/Cargo.toml
[dependencies]
shiguredo_opus = { version = "=2026.2.0", default-features = false }   # DRED 不要
```

`shiguredo_audio_device` の feature は **ターゲット別**に有効化する(`oto-capture/Cargo.toml`):

```toml
[target.'cfg(target_os = "macos")'.dependencies]
shiguredo_audio_device = { version = "2026.3", default-features = false, features = ["coreaudio", "default-coreaudio"] }

[target.'cfg(target_os = "linux")'.dependencies]
shiguredo_audio_device = { version = "2026.3", default-features = false, features = ["pulse", "default-pulse"] }

[target.'cfg(target_os = "windows")'.dependencies]
shiguredo_audio_device = { version = "2026.3", default-features = false, features = ["wasapi", "default-wasapi"] }
```

default feature(全 backend + `windows` crate)を有効にすると、非 Windows ビルドでも `windows`
クレートが依存グラフに入り無駄にビルド時間を食うため、ターゲット別 feature を採用する。

## package.nix(oto)

koe/sui のパターンに「**cp + autoPatchelfHook**」を足した形:

```nix
{ stdenv, craneLib, src, version, cargoArtifacts, libpulse, autoPatchelfHook }:
let
  workspaceBuild = craneLib.buildPackage {
    inherit src version cargoArtifacts;
    pname = "labs-workspace";
    strictDeps = true;
    doCheck = false;   # テストは flake の checks.test が担当
  };
in
stdenv.mkDerivation {
  pname = "oto";
  inherit version;
  src = workspaceBuild;
  dontConfigure = true;
  dontBuild = true;
  dontUnpack = true;
  nativeBuildInputs = pkgs.lib.optionals stdenv.isLinux [ autoPatchelfHook ];
  buildInputs = pkgs.lib.optionals stdenv.isLinux [ libpulse ];
  installPhase = ''
    runHook preInstall
    mkdir -p $out/bin
    cp $src/bin/oto $out/bin/oto   # symlink だと autoPatchelf が対象外になるため cp
    runHook postInstall
  '';
  meta = {
    mainProgram = "oto";
    description = "Oto (音) — cross-platform offline microphone recorder (WAV / Ogg+Opus)";
    homepage = "https://github.com/pranc1ngpegasus/labs/tree/main/oto";
  };
}
```

flake.nix の `packages` に `oto = import ./oto/package.nix { inherit (pkgs) stdenv libpulse autoPatchelfHook; inherit craneLib src version cargoArtifacts; };` を追加し、`default`(symlinkJoin)にも加える。

## devShell

ローカル開発(`cargo build`)でも同じ前提が要る: Linux の devShell に `pkg-config`・
`libpulse.dev`・`llvmPackages.libclang` と `LIBCLANG_PATH` env を追加する(現状 devShell は
rustToolchain のみ)。Windows 開発者は plain Cargo でよく、ネットワークがあれば
prebuilt libopus を build.rs が自動取得する。

## CI での検証

- 既存 workflow(`.github/workflows/nix.yaml`)は ubuntu-latest で `nix flake check`。
  x86_64-linux の workspace 全体ビルドに oto が含まれ、PulseAudio バックエンド + bindgen +
  libopus FOD が **CI で実際に検証される**。
- 一方 **aarch64-darwin / aarch64-linux は CI 対象外**(flake の declared systems のみ)なので、
  手元で `nix build .#oto --system aarch64-darwin` 等による確認が必要。macOS ランナーでの
  CI 追加は将来課題として計画 07 に記載する。

## アップグレード手順(shiguredo_opus)

1. `Cargo.toml` の `=X.Y.Z` を更新し `cargo update -p shiguredo_opus`。
2. リリースアセットの 3 つの sha256 を取得して FOD を更新。
3. パッチが新しいソースに適用できるか確認(差分が大きくなったら再作成)。

## ライセンスの注意

- `shiguredo_opus` は Apache-2.0 だが、**静的リンクされる libopus は Xiph の BSD 系ライセンス**。
  バイナリ配布時は libopus の著作権表示・ライセンス条項の保持が必要。
  リポジトリでは `oto/LICENSE` または README に THIRD-PARTY 注記を置く(計画 07 に含める)。
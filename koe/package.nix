# koe/package.nix
#
# Builds the `koe` CLI from the enclosing cargo workspace and exposes it as a
# standalone derivation (`packages.<system>.koe`).
#
# The whole workspace is built in a single derivation and `bin/koe` is then
# symlinked into a dedicated output. Do NOT switch to `-p koe-cli`: cargo
# resolves a *subset* of the workspace's feature set for a single package, so
# the shared `buildDepsOnly` artifact no longer matches the final build and
# every build recompiles — the dependency cache stops working.
#
# Test execution is owned by the root flake's `checks` (workspace-wide
# `cargo test`); running tests here as well would only recompile and re-run
# them, hence `doCheck = false`.
{
  stdenv,
  craneLib, # already overridden with the repo's toolchain (rust-overlay)
  src, # filtered workspace root (must contain Cargo.toml + Cargo.lock)
  cargoArtifacts, # workspace dependency artifacts, built once in the root flake
  cargoVendorDir, # vendored deps with the shiguredo_opus build.rs patch applied
  audioNativeBuildInputs, # pkg-config + libclang (bindgen)
  audioBuildInputs, # libpulse.dev on Linux
  audioEnv, # LIBCLANG_PATH / OPUS_TARGET / OPUS_PREBUILT_TARBALL
  version ? "0.0.0",
}:
let
  # Builds every binary in the workspace (ren/koe/sui once they are members).
  workspaceBuild = craneLib.buildPackage {
    inherit
      src
      version
      cargoArtifacts
      cargoVendorDir
      ;
    pname = "labs-workspace";
    strictDeps = true;
    nativeBuildInputs = audioNativeBuildInputs;
    buildInputs = audioBuildInputs;
    env = audioEnv;
    doCheck = false;
  };
in
stdenv.mkDerivation {
  pname = "koe";
  inherit version;

  src = workspaceBuild;

  dontConfigure = true;
  dontBuild = true;
  dontUnpack = true;

  installPhase = ''
    runHook preInstall
    mkdir -p $out/bin
    ln -s $src/bin/koe $out/bin/koe
    runHook postInstall
  '';

  meta = {
    mainProgram = "koe";
    description = "Koe (声) — macOS offline transcription & recording tool. Captures system audio via Core Audio Process Tap and ScreenCaptureKit, transcribes on-device, and exposes a CLI/FFI.";
    homepage = "https://github.com/pranc1ngpegasus/labs/tree/main/koe";
  };
}

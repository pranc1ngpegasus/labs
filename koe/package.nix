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
  version ? "0.0.0",
}:
let
  commonArgs = {
    inherit src version;
    pname = "labs-workspace";
    strictDeps = true;
  };

  cargoArtifacts = craneLib.buildDepsOnly commonArgs;

  # Builds every binary in the workspace (ren/koe/sui once they are members).
  workspaceBuild = craneLib.buildPackage (
    commonArgs
    // {
      inherit cargoArtifacts;
      doCheck = false;
    }
  );
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

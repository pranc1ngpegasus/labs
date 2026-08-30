# sui/package.nix
#
# Builds the `sui` CLI from the enclosing cargo workspace and exposes it as a
# standalone derivation (`packages.<system>.sui`).
#
# The whole workspace is built in a single derivation and `bin/sui` is then
# symlinked into a dedicated output. Do NOT switch to `-p sui`: cargo resolves
# a *subset* of the workspace's feature set for a single package (e.g.
# `sui-workflow` pulls `rhai`, which enables `once_cell`'s `portable-atomic`),
# so the shared `buildDepsOnly` artifact no longer matches the final build and
# every build recompiles — the dependency cache stops working.
#
# Test execution is owned by the root flake's `checks.test` (workspace-wide
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
  pname = "sui";
  inherit version;

  src = workspaceBuild;

  dontConfigure = true;
  dontBuild = true;
  dontUnpack = true;

  installPhase = ''
    runHook preInstall
    mkdir -p $out/bin
    ln -s $src/bin/sui $out/bin/sui
    runHook postInstall
  '';

  meta = {
    mainProgram = "sui";
    description = "Coding agent — 粋・推・遂. A lightweight, robust coding agent with a workflow engine and memory features.";
    homepage = "https://github.com/pranc1ngpegasus/labs/tree/main/sui";
  };
}

# oto/package.nix
#
# Builds the `oto` CLI from the enclosing cargo workspace and exposes it as a
# standalone derivation (`packages.<system>.oto`).
#
# The whole workspace is built in a single derivation and `bin/oto` is then
# copied into a dedicated output. The binary is symlinked in the sibling
# projects but must be *copied* here: on Linux the dynamically-linked
# `libpulse.so.0` needs its rpath patched by `autoPatchelfHook`, which only
# processes regular files.
#
# Test execution is owned by the root flake's `checks` (workspace-wide
# `cargo test`); running tests here as well would only recompile and re-run
# them, hence `doCheck = false`.
{
  stdenv,
  craneLib, # already overridden with the repo's toolchain (rust-overlay)
  src, # filtered workspace root (must contain Cargo.toml + Cargo.lock)
  cargoArtifacts, # workspace dependency artifacts, built once in the root flake
  autoPatchelfHook, # Linux only: rpath for the dynamic PulseAudio client lib
  libpulse, # Linux only: runtime dependency of the PulseAudio backend
  version ? "0.0.0",
}:
let
  # Builds every binary in the workspace (ren/koe/sui/oto once they are members).
  workspaceBuild = craneLib.buildPackage {
    inherit src version cargoArtifacts;
    pname = "labs-workspace";
    strictDeps = true;
    doCheck = false;
  };
in
stdenv.mkDerivation {
  pname = "oto";
  inherit version;

  src = workspaceBuild;

  dontConfigure = true;
  dontBuild = true;
  dontUnpack = true;

  nativeBuildInputs = if stdenv.isLinux then [ autoPatchelfHook ] else [ ];
  buildInputs = if stdenv.isLinux then [ libpulse ] else [ ];

  installPhase = ''
    runHook preInstall
    mkdir -p $out/bin
    cp $src/bin/oto $out/bin/oto
    runHook postInstall
  '';

  meta = {
    mainProgram = "oto";
    description = "Oto (音) — cross-platform offline microphone recorder (WAV / Ogg+Opus)";
    homepage = "https://github.com/pranc1ngpegasus/labs/tree/main/oto";
  };
}
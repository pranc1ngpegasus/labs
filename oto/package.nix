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
# Unlike the sibling projects, the workspace build needs oto's build-time
# prerequisites (design 05): the patched `cargoVendorDir` (offline libopus),
# bindgen/libpulse inputs, and the `OPUS_PREBUILT_TARBALL`/`LIBCLANG_PATH`
# environment. These are the same values the root flake passes to
# `commonArgs`; without them the final build re-runs dependency build scripts
# against an unpatched vendor tree.
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
  autoPatchelfHook, # Linux only: rpath for the dynamic PulseAudio client lib
  libpulseaudio, # Linux only: runtime dependency of the PulseAudio backend
  version ? "0.0.0",
}:
let
  # Builds every binary in the workspace (ren/koe/sui/oto once they are members).
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
  pname = "oto";
  inherit version;

  src = workspaceBuild;

  dontConfigure = true;
  dontBuild = true;
  dontUnpack = true;

  nativeBuildInputs = if stdenv.hostPlatform.isLinux then [ autoPatchelfHook ] else [ ];
  buildInputs = if stdenv.hostPlatform.isLinux then [ libpulseaudio ] else [ ];

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

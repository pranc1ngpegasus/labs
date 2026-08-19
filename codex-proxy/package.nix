# codex-proxy/package.nix
#
# Builds the `codex-proxy` daemon from the enclosing cargo workspace and
# exposes it as a standalone derivation (`packages.<system>.codex-proxy`).
#
# The whole workspace is built in a single derivation and `bin/codex-proxy` is
# then symlinked into a dedicated output (same pattern as sui/package.nix).
#
# Test execution is owned by the root flake's `checks.test` (workspace-wide
# `cargo test`), so `doCheck = false` here.
{
  stdenv,
  craneLib, # already overridden with the repo's toolchain (rust-overlay)
  src, # filtered workspace root (must contain Cargo.toml + Cargo.lock)
  cargoArtifacts, # workspace dependency artifacts, built once in the root flake
  version ? "0.0.0",
}:
let
  workspaceBuild = craneLib.buildPackage {
    inherit src version cargoArtifacts;
    pname = "labs-workspace";
    strictDeps = true;
    doCheck = false;
  };
in
stdenv.mkDerivation {
  pname = "codex-proxy";
  inherit version;

  src = workspaceBuild;

  dontConfigure = true;
  dontBuild = true;
  dontUnpack = true;

  installPhase = ''
    runHook preInstall
    mkdir -p $out/bin
    ln -s $src/bin/codex-proxy $out/bin/codex-proxy
    runHook postInstall
  '';

  meta = {
    mainProgram = "codex-proxy";
    description = "常駐 OAuth プロキシ — Codex の ChatGPT トークンを自動更新し OpenAI-compatible な /v1 を expose する";
    homepage = "https://github.com/pranc1ngpegasus/labs/tree/main/codex-proxy";
  };
}

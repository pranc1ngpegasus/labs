{
  description = "collection of my personal open-source projects and experiments";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-parts.url = "github:hercules-ci/flake-parts";
    flake-parts.inputs.nixpkgs-lib.follows = "nixpkgs";
    git-hooks.url = "github:cachix/git-hooks.nix";
    git-hooks.inputs.nixpkgs.follows = "nixpkgs";
    treefmt-nix.url = "github:numtide/treefmt-nix";
    treefmt-nix.inputs.nixpkgs.follows = "nixpkgs";
    rust-overlay.url = "github:oxalica/rust-overlay";
    rust-overlay.inputs.nixpkgs.follows = "nixpkgs";
    crane.url = "github:ipetkov/crane";
  };

  outputs =
    inputs:
    inputs.flake-parts.lib.mkFlake { inherit inputs; } {
      imports = [
        inputs.git-hooks.flakeModule
        inputs.treefmt-nix.flakeModule
      ];

      flake.homeManagerModules.codex-proxy = ./codex-proxy/home-manager.nix;

      perSystem =
        {
          config,
          pkgs,
          system,
          ...
        }:
        let
          version =
            let
              d = inputs.self.lastModifiedDate;
              dezero =
                s:
                let
                  m = builtins.match "0(.*)" s;
                in
                if m == null then s else builtins.head m;
              date = "${builtins.substring 0 4 d}.${dezero (builtins.substring 4 2 d)}.${
                dezero (builtins.substring 6 2 d)
              }";
              rev = inputs.self.shortRev or inputs.self.dirtyShortRev or "dirty";
            in
            "${date}-${rev}";

          rustToolchain = pkgs.rust-bin.stable.latest.default;
          craneLib = (inputs.crane.mkLib pkgs).overrideToolchain rustToolchain;
          src = pkgs.lib.cleanSourceWith {
            # cleanCargoSource would strip the non-Rust files that the ren
            # crates pull in via include_str! (their assets/ and bundled/
            # dirs, plus ren/README.md used by test code) — keep those.
            src = ./.;
            filter =
              path: type:
              let
                p = toString path;
              in
              builtins.match ".*/(bundled|assets)(/.*)?$" p != null
              || pkgs.lib.hasSuffix "/README.md" p
              || craneLib.filterCargoSources path type;
          };

          # oto build prerequisites (design 05):
          # - shiguredo_audio_device: cc + bindgen shims. Linux needs
          #   PulseAudio headers (pkg-config) and libclang; macOS needs
          #   libclang too (bindgen) while the frameworks come from the SDK.
          # - shiguredo_opus: build.rs downloads a prebuilt libopus with curl,
          #   which cannot work in a network-isolated sandbox. The build script
          #   is patched (cargoPatches) to accept an injected tarball, fetched
          #   here as a fixed-output derivation (per release asset sha256).
          opusTargets = {
            x86_64-linux = {
              asset = "libopus-ubuntu-24.04_x86_64.tar.gz";
              sha256 = "c028f032718147b82c3ba4a4148548c95106717f081f32ad14a2de3b864e6f8f";
              opusTarget = "ubuntu_24.04_x86_64";
            };
            aarch64-linux = {
              asset = "libopus-ubuntu-24.04_arm64.tar.gz";
              sha256 = "129f4d6ccbb8598f97d8ba77fab5db431ead7151f3b86a6253fc55a4c441c449";
              opusTarget = "ubuntu_24.04_arm64";
            };
            aarch64-darwin = {
              asset = "libopus-macos_arm64.tar.gz";
              sha256 = "ebb588a606c050744cc673159df77b256686650506f3f7eb5111dbf40a6000ad";
              opusTarget = "macos_arm64";
            };
          };
          opusPrebuilt = pkgs.fetchurl {
            url = "https://github.com/shiguredo/opus-rs/releases/download/2026.2.0/${opusTargets.${system}.asset}";
            sha256 = opusTargets.${system}.sha256;
          };
          audioNativeBuildInputs = [
            pkgs.pkg-config
            pkgs.llvmPackages.libclang
          ];
          audioBuildInputs = pkgs.lib.optionals pkgs.stdenv.hostPlatform.isLinux [ pkgs.libpulse.dev ];
          audioEnv = {
            LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
            # get_target_platform() panics on non-Ubuntu systems; the value is
            # only used to name the (bypassed) download URL.
            OPUS_TARGET = opusTargets.${system}.opusTarget;
            OPUS_PREBUILT_TARBALL = "${opusPrebuilt}";
          };
          # crane's `cargoPatches` do not touch transitive (vendored) crates, so
          # the build.rs change is applied here: `overrideVendorCargoPackage`
          # patches the vendored crate source. This is safe because crane writes
          # an empty `{"files":{}}` checksum map, so no checksum revalidation
          # happens after patching.
          opusPatch = ./oto/nix/patches/shiguredo_opus-prebuilt.patch;
          overrideVendorCargoPackage =
            p: drv:
            if p.name == "shiguredo_opus" then
              pkgs.runCommandLocal "cargo-package-shiguredo_opus-patched" { } ''
                cp -a ${drv} $out
                chmod -R u+w $out
                patch -d $out -p1 < ${opusPatch}
              ''
            else
              drv;
          # The override hook must not leak into the derivation env, so build
          # the vendored source tree here and pass it down as `cargoVendorDir`.
          cargoVendorDir = craneLib.vendorCargoDeps {
            inherit src overrideVendorCargoPackage;
          };

          # The whole workspace builds in a single derivation (see the
          # package.nix files), so dependency artifacts are built once here
          # and shared by packages and checks alike.
          #
          # Dependency artifacts only depend on Cargo.lock, not on the
          # workspace version. Pin a stable version so the date/git-rev baked
          # into `version` never invalidates this (expensive) deps build — it
          # would otherwise rebuild on every commit and on every file save
          # while the tree is dirty (`dirtyShortRev` changes each edit).
          commonArgs = {
            inherit src cargoVendorDir;
            pname = "labs-workspace";
            version = "0.0.0";
            strictDeps = true;
            nativeBuildInputs = audioNativeBuildInputs;
            buildInputs = audioBuildInputs;
            env = audioEnv;
          };
          cargoArtifacts = craneLib.buildDepsOnly commonArgs;
        in
        {
          _module.args.pkgs = import inputs.nixpkgs {
            inherit system;
            overlays = [ inputs.rust-overlay.overlays.default ];
          };

          checks = {
            fmt = craneLib.cargoFmt {
              inherit src;
              pname = "labs-workspace";
            };
            clippy = craneLib.cargoClippy {
              inherit src cargoArtifacts cargoVendorDir;
              pname = "labs-workspace";
              cargoClippyExtraArgs = "--all-targets --all-features -- --deny warnings";
              nativeBuildInputs = audioNativeBuildInputs;
              buildInputs = audioBuildInputs;
              env = audioEnv;
            };
            hakari = craneLib.mkCargoDerivation {
              inherit src;
              pname = "labs-hakari";
              cargoArtifacts = null;
              doInstallCargoArtifacts = false;
              buildPhaseCargoCommand = ''
                cargo hakari generate --diff
                cargo hakari manage-deps --dry-run
                cargo hakari verify
              '';
              nativeBuildInputs = [
                pkgs.cargo-hakari
              ];
            };
            test = craneLib.cargoTest {
              inherit src cargoArtifacts cargoVendorDir;
              pname = "labs-workspace";
              # sui-tools' edit tests run `git init` in a temp dir; oto's test
              # binaries link the dynamic PulseAudio client library on Linux.
              nativeBuildInputs = [ pkgs.git ] ++ audioNativeBuildInputs;
              buildInputs = audioBuildInputs;
              env = audioEnv;
            };
          };

          devShells.default = pkgs.mkShellNoCC {
            inputsFrom = [ config.pre-commit.devShell ];

            packages = [ rustToolchain ];
          };

          packages = {
            codex-proxy = import ./codex-proxy/package.nix {
              inherit (pkgs) stdenv;
              inherit
                craneLib
                src
                version
                cargoArtifacts
                ;
            };
            sui = import ./sui/package.nix {
              inherit (pkgs) stdenv;
              inherit
                craneLib
                src
                version
                cargoArtifacts
                ;
            };
            koe = import ./koe/package.nix {
              inherit (pkgs) stdenv;
              inherit
                craneLib
                src
                version
                cargoArtifacts
                ;
            };
            ren = import ./ren/package.nix {
              inherit (pkgs) stdenv;
              inherit
                craneLib
                src
                version
                cargoArtifacts
                ;
            };
            oto = import ./oto/package.nix {
              inherit (pkgs) stdenv libpulse autoPatchelfHook;
              inherit
                craneLib
                src
                version
                cargoArtifacts
                cargoVendorDir
                audioNativeBuildInputs
                audioBuildInputs
                audioEnv
                ;
            };
            default = pkgs.symlinkJoin {
              name = "labs-all";
              paths = [
                config.packages.codex-proxy
                config.packages.sui
                config.packages.koe
                config.packages.ren
                config.packages.oto
              ];
            };
          };

          pre-commit.settings = {
            hooks = {
              actionlint.enable = true;
              deadnix.enable = true;
              statix.enable = true;
              statix.excludes = [
                ".direnv"
              ];
            };
          };

          treefmt = {
            projectRootFile = "flake.nix";
            programs = {
              nixfmt.enable = true;
              rustfmt.enable = true;
              rustfmt.package = rustToolchain;
              swift-format.enable = true;
              taplo.enable = true;
              yamlfmt.enable = true;
            };
            settings.global.excludes = [
              "koe/koe-native/generated/**"
              "workspace-hack/Cargo.toml"
            ];
          };
        };

      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "aarch64-darwin"
      ];
    };
}

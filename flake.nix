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
            inherit src;
            pname = "labs-workspace";
            version = "0.0.0";
            strictDeps = true;
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
              inherit src cargoArtifacts;
              pname = "labs-workspace";
              cargoClippyExtraArgs = "--all-targets --all-features -- --deny warnings";
            };
            test = craneLib.cargoTest {
              inherit src cargoArtifacts;
              pname = "labs-workspace";
              # sui-tools' edit tests run `git init` in a temp dir.
              nativeBuildInputs = [ pkgs.git ];
            };
          };

          devShells.default = pkgs.mkShellNoCC {
            inputsFrom = [ config.pre-commit.devShell ];

            packages = [ rustToolchain ];
          };

          packages = {
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
            default = pkgs.symlinkJoin {
              name = "labs-all";
              paths = [
                config.packages.sui
                config.packages.koe
                config.packages.ren
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

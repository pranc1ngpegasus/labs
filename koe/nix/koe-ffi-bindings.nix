# FFI bindings pipeline: koe-ffi staticlib → uniffi-bindgen → Swift/C artifacts.
{
  lib,
  pkgs,
  craneLib,
  args,
}:
let
  koeFfiBindings = craneLib.cargoBuild (
    args
    // {
      pname = "koe-ffi-bindings";
      version = "0.0.0";
      cargoBuildProfile = "release";
      nativeBuildInputs = [ pkgs.swift-format ];
      dontInstall = false;
      installPhase = ''
        runHook preInstall

        mkdir -p "$out/lib" "$out/include"
        install -Dm644 target/release/libkoe_ffi.a "$out/lib/libkoe_ffi.a"

        cargo run --release -p koe-ffi --bin uniffi-bindgen -- generate \
          --library "$PWD/target/release/libkoe_ffi.a" \
          --language swift \
          --out-dir "$out/include" \
          --no-format \
          --metadata-no-deps

        swift-format format --in-place "$out/include/koe_ffi.swift"

        runHook postInstall
      '';
    }
  );

  populateScript = pkgs.writeShellApplication {
    name = "populate-koe-ffi-bindings";
    runtimeInputs = [ pkgs.coreutils ];
    text = ''
       if root="$(git rev-parse --show-toplevel 2>/dev/null)"; then
         :
       else
         root="$(cd "$(dirname "$0")/../.." && pwd)"
       fi
      mkdir -p "$root/koe-native/generated" "$root/target/debug"
      cp -r ${koeFfiBindings}/include/* "$root/koe-native/generated/"
      install -m644 ${koeFfiBindings}/lib/libkoe_ffi.a "$root/target/debug/libkoe_ffi.a"
      echo "Populated $root/koe-native/generated and target/debug/libkoe_ffi.a"
    '';
  };

  koeFfiBindingsCheck =
    pkgs.runCommand "koe-ffi-bindings-check"
      {
        nativeBuildInputs = [
          pkgs.swift
          pkgs.clang
        ];
      }
      ''
        swiftc -typecheck \
          -module-name KoeFfi \
          -emit-module -emit-module-path "$TMPDIR/KoeFfi.swiftmodule" \
          ${koeFfiBindings}/include/koe_ffi.swift \
          -I ${koeFfiBindings}/include \
          -Xcc -fmodule-map-file=${koeFfiBindings}/include/koe_ffiFFI.modulemap

        clang -fsyntax-only \
          -fmodule-map-file=${koeFfiBindings}/include/koe_ffiFFI.modulemap \
          ${koeFfiBindings}/include/koe_ffiFFI.h

        mkdir -p "$out"
        touch "$out/ok"
      '';

in
{
  packages = {
    koe-ffi-bindings = koeFfiBindings;
    koe-ffi-populate = populateScript;
  };

  checks = lib.optionalAttrs pkgs.stdenv.isDarwin {
    koe-ffi-bindings = koeFfiBindingsCheck;
  };

  devShellHook = lib.optionalString pkgs.stdenv.isDarwin ''
    if [ -z "''${KOE_FFI_SKIP_POPULATE:-}" ]; then
      ${populateScript}/bin/populate-koe-ffi-bindings
    fi
  '';
}

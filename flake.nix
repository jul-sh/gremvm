{
  description = "A persistent Lume-managed macOS VM";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-26.05-darwin";

  outputs =
    { nixpkgs, ... }:
    let
      system = "aarch64-darwin";
      pkgs = import nixpkgs { inherit system; };
      projectFiles = nixpkgs.lib.fileset.unions [
        ./Cargo.toml
        ./Cargo.lock
        ./LICENSE
        ./README.md
        ./src
        ./tests
        ./guest
      ];
      gremvmSource = nixpkgs.lib.fileset.toSource {
        root = ./.;
        fileset = projectFiles;
      };
      checkSource = nixpkgs.lib.fileset.toSource {
        root = ./.;
        fileset = nixpkgs.lib.fileset.union projectFiles ./flake.nix;
      };
      lumeVersion = "0.4.0";
      lume = pkgs.stdenvNoCC.mkDerivation {
        pname = "lume";
        version = lumeVersion;
        src = pkgs.fetchurl {
          url = "https://github.com/trycua/cua/releases/download/lume-v${lumeVersion}/lume-${lumeVersion}-darwin-arm64.tar.gz";
          hash = "sha256-i0S7zFrpaT9LE0P+pYqt3dNwU/qZDNI05wPIyec7HLo=";
        };
        sourceRoot = ".";
        nativeBuildInputs = [ pkgs.makeWrapper ];
        dontBuild = true;
        dontFixup = true;
        installPhase = ''
          runHook preInstall
          mkdir -p "$out/Applications" "$out/bin" "$out/share/lume"
          cp -R lume.app "$out/Applications/lume.app"
          makeWrapper "$out/Applications/lume.app/Contents/MacOS/lume" "$out/bin/lume"
          install -m 0444 ${
            pkgs.fetchurl {
              url = "https://raw.githubusercontent.com/trycua/cua/ee15ae942cefe809fd97a565220eca9c6a295ac0/LICENSE.md";
              hash = "sha256-wHeSkMHUeDFpqj2/tV/rUF5WPvigBLv1UpjO/8+9qNk=";
            }
          } "$out/share/lume/LICENSE.md"
          runHook postInstall
        '';
        meta = {
          description = "macOS and Linux virtual machines on Apple Silicon";
          homepage = "https://cua.ai/docs/lume";
          license = pkgs.lib.licenses.mit;
          mainProgram = "lume";
          platforms = [ system ];
        };
      };
      gremvmBin = pkgs.rustPlatform.buildRustPackage {
        pname = "gremvm";
        version = "0.1.0";
        src = gremvmSource;
        cargoLock.lockFile = ./Cargo.lock;
      };
      gremvm =
        pkgs.runCommand "gremvm"
          {
            meta = {
              description = "Manage one persistent Lume macOS VM";
              license = pkgs.lib.licenses.mit;
              mainProgram = "gremvm";
              platforms = [ system ];
            };
          }
          ''
            mkdir -p "$out/Applications" "$out/bin" "$out/share/gremvm"
            ln -s ${lume}/Applications/lume.app "$out/Applications/lume.app"
            ln -s ${lume}/bin/lume "$out/bin/lume"
            install -m 0755 ${gremvmBin}/bin/gremvm "$out/bin/gremvm"
            install -m 0755 ${./guest/guest-setup.sh} "$out/share/gremvm/guest-setup.sh"
          '';
      gremvmApp = {
        type = "app";
        program = "${gremvm}/bin/gremvm";
        meta.description = "Manage one persistent Lume macOS VM";
      };
    in
    {
      packages.${system} = {
        default = gremvm;
        inherit gremvm;
      };

      apps.${system} = {
        default = gremvmApp;
        gremvm = gremvmApp;
      };

      devShells.${system}.default = pkgs.mkShell {
        packages = [
          pkgs.cargo
          pkgs.clippy
          pkgs.nixfmt
          pkgs.rustc
          pkgs.rustfmt
          gremvm
        ];
      };

      formatter.${system} = pkgs.nixfmt;

      checks.${system} = {
        package = gremvm;
        static = pkgs.stdenv.mkDerivation {
          pname = "gremvm-static-checks";
          version = "0.1.0";
          src = checkSource;
          cargoDeps = pkgs.rustPlatform.importCargoLock {
            lockFile = ./Cargo.lock;
          };
          nativeBuildInputs = [
            pkgs.bash
            pkgs.cargo
            pkgs.clippy
            pkgs.nixfmt
            pkgs.rustPlatform.cargoSetupHook
            pkgs.rustc
            pkgs.rustfmt
          ];
          dontConfigure = true;
          buildPhase = ''
            runHook preBuild
            cargo fmt --check
            cargo clippy --all-targets --locked --offline -- -D warnings
            cargo test --all-targets --locked --offline
            bash -n guest/guest-setup.sh
            nixfmt --check flake.nix
            runHook postBuild
          '';
          installPhase = ''
            runHook preInstall
            touch "$out"
            runHook postInstall
          '';
        };
        bundle = pkgs.runCommand "gremvm-bundle-check" { } ''
          test -x ${gremvm}/bin/gremvm
          test -x ${gremvm}/bin/lume
          test "$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' ${gremvm}/Applications/lume.app/Contents/Info.plist)" = ${lumeVersion}
          /usr/bin/codesign --verify --deep --strict ${gremvm}/Applications/lume.app
          /usr/bin/codesign -d --xml --entitlements "$TMPDIR/lume-entitlements.plist" ${gremvm}/Applications/lume.app
          test "$(/usr/libexec/PlistBuddy -c 'Print :com.apple.security.virtualization' "$TMPDIR/lume-entitlements.plist")" = true
          test "$(/usr/libexec/PlistBuddy -c 'Print :com.apple.vm.networking' "$TMPDIR/lume-entitlements.plist")" = true
          test -f ${gremvm}/Applications/lume.app/Contents/Resources/lume_lume.bundle/unattended-presets/tahoe.yml
          test -x ${gremvm}/share/gremvm/guest-setup.sh
          mkdir -p "$TMPDIR/home"
          HOME="$TMPDIR/home" ${gremvm}/bin/gremvm --help >/dev/null
          touch "$out"
        '';
      };
    };
}

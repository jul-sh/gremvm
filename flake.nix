{
  description = "A persistent Tart-managed macOS VM";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-26.05-darwin";

  outputs =
    { nixpkgs, ... }:
    let
      system = "aarch64-darwin";
      pkgs = import nixpkgs {
        inherit system;
        config.allowUnfreePredicate =
          pkg:
          builtins.elem (nixpkgs.lib.getName pkg) [
            "packer"
            "tart"
          ];
      };
      projectFiles = nixpkgs.lib.fileset.unions [
        ./Cargo.toml
        ./Cargo.lock
        ./LICENSE
        ./README.md
        ./src
        ./tests
        ./packer
      ];
      gremvmSource = nixpkgs.lib.fileset.toSource {
        root = ./.;
        fileset = projectFiles;
      };
      checkSource = nixpkgs.lib.fileset.toSource {
        root = ./.;
        fileset = nixpkgs.lib.fileset.union projectFiles ./flake.nix;
      };
      pluginVersion = "1.21.0";
      pluginExecutable = "packer-plugin-tart_v${pluginVersion}_x5.0_darwin_arm64";
      packerPluginTart = pkgs.stdenvNoCC.mkDerivation {
        pname = "packer-plugin-tart";
        version = pluginVersion;
        src = pkgs.fetchurl {
          url = "https://github.com/cirruslabs/packer-plugin-tart/releases/download/v${pluginVersion}/${pluginExecutable}.zip";
          hash = "sha256-SjTKh7VANinaXKSTjpTupb8ygF+vA0ohGDViW2N9TnY=";
        };
        nativeBuildInputs = [ pkgs.unzip ];
        dontUnpack = true;
        installPhase = ''
          plugin_dir="$out/libexec/packer/plugins/github.com/cirruslabs/tart"
          mkdir -p "$plugin_dir"
          unzip -p "$src" > "$plugin_dir/${pluginExecutable}"
          chmod 0755 "$plugin_dir/${pluginExecutable}"
          sha256sum "$plugin_dir/${pluginExecutable}" \
            | cut -d ' ' -f 1 \
          > "$plugin_dir/${pluginExecutable}_SHA256SUM"
        '';
      };
      tartVersion = "2.34.0";
      tart = pkgs.stdenvNoCC.mkDerivation {
        pname = "tart";
        version = tartVersion;
        src = pkgs.fetchurl {
          url = "https://github.com/openai/tart/releases/download/${tartVersion}/tart.tar.gz";
          hash = "sha256-yfFgn0lFJY7w7id91E3JcA1vBpeJoR5Dvn81sKZLMTU=";
        };
        sourceRoot = ".";
        nativeBuildInputs = [ pkgs.makeWrapper ];
        dontBuild = true;
        dontFixup = true;
        installPhase = ''
          runHook preInstall
          mkdir -p "$out/Applications" "$out/bin" "$out/share/tart"
          cp -R tart.app "$out/Applications/tart.app"
          makeWrapper "$out/Applications/tart.app/Contents/MacOS/tart" "$out/bin/tart"
          install -m 0444 LICENSE "$out/share/tart/LICENSE"
          runHook postInstall
        '';
        meta = pkgs.tart.meta // {
          license = pkgs.lib.licenses.fsl11Asl20;
        };
      };
      gremvmBin = pkgs.rustPlatform.buildRustPackage {
        pname = "gremvm";
        version = "0.1.0";
        src = gremvmSource;
        cargoLock.lockFile = ./Cargo.lock;
      };
      gremvm = pkgs.symlinkJoin {
        name = "gremvm";
        paths = [
          pkgs.packer
          packerPluginTart
        ];
        postBuild = ''
          install -m 0755 ${gremvmBin}/bin/gremvm "$out/bin/gremvm"
          ln -s ${tart}/bin/tart "$out/bin/tart"
          mkdir -p "$out/Applications"
          ln -s ${tart}/Applications/tart.app "$out/Applications/tart.app"
          mkdir -p "$out/share/gremvm"
          install -m 0644 ${./packer/gremvm.pkr.hcl} "$out/share/gremvm/gremvm.pkr.hcl"
          install -m 0644 ${./packer/auto-login.pl} "$out/share/gremvm/auto-login.pl"
          mkdir -p "$out/libexec/gremvm" "$out/share/gremvm/licenses"
          install -m 0755 ${pkgs.tailscale}/bin/.tailscaled-wrapped \
            "$out/libexec/gremvm/tailscaled"
          install -m 0644 ${pkgs.tailscale.src}/LICENSE \
            "$out/share/gremvm/licenses/tailscale.txt"
        '';
        meta = {
          description = "Manage one persistent Tart macOS VM";
          license = [
            pkgs.lib.licenses.mit
            pkgs.lib.licenses.bsd3
          ];
          mainProgram = "gremvm";
          platforms = [ system ];
        };
      };
      gremvmApp = {
        type = "app";
        program = "${gremvm}/bin/gremvm";
        meta.description = "Manage one persistent Tart macOS VM";
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
            pkgs.cargo
            pkgs.clippy
            pkgs.nixfmt
            pkgs.packer
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
            nixfmt --check flake.nix
            packer fmt -check packer/gremvm.pkr.hcl
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
          test -x ${gremvm}/bin/tart
          test "$(${gremvm}/bin/tart --version)" = ${tartVersion}
          /usr/bin/codesign --verify --deep --strict ${gremvm}/Applications/tart.app
          test -x ${gremvm}/bin/packer
          test -f ${gremvm}/share/gremvm/gremvm.pkr.hcl
          test -f ${gremvm}/share/gremvm/auto-login.pl
          test -x ${gremvm}/libexec/gremvm/tailscaled
          test -f ${gremvm}/share/gremvm/licenses/tailscale.txt
          test "$(${gremvm}/libexec/gremvm/tailscaled --version | head -n 1)" = ${pkgs.tailscale.version}
          /usr/bin/codesign --verify --strict ${gremvm}/libexec/gremvm/tailscaled
          test -f ${gremvm}/libexec/packer/plugins/github.com/cirruslabs/tart/${pluginExecutable}
          HOME="$TMPDIR/home" ${gremvm}/bin/gremvm --help >/dev/null
          mkdir -p "$TMPDIR/home" "$TMPDIR/packer"
          cp ${gremvm}/share/gremvm/gremvm.pkr.hcl "$TMPDIR/packer/gremvm.pkr.hcl"
          cp ${gremvm}/share/gremvm/auto-login.pl "$TMPDIR/packer/auto-login.pl"
          cd "$TMPDIR/packer"
          export HOME="$TMPDIR/home"
          export PACKER_NO_COLOR=1
          export PACKER_PLUGIN_PATH=${gremvm}/libexec/packer/plugins
          export CHECKPOINT_DISABLE=1
          PKR_VAR_vm_name=gremvm \
            PKR_VAR_cpu_count=6 \
            PKR_VAR_memory_gb=24 \
            PKR_VAR_disk_size_gb=192 \
            PKR_VAR_ssh_public_key="ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAITest" \
            PKR_VAR_guest_password=0123456789abcdef0123456789abcdef0123456789abcdef \
            ${gremvm}/bin/packer validate gremvm.pkr.hcl
          touch "$out"
        '';
      };
    };
}

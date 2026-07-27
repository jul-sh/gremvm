{
  description = "Pinned tooling for a Tart-backed persistent macOS VM";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-26.05-darwin";
  };

  outputs =
    {
      self,
      nixpkgs,
    }:
    let
      lib = nixpkgs.lib;
      systems = [ "aarch64-darwin" ];
      forAllSystems = lib.genAttrs systems;
      pkgsFor =
        system:
        import nixpkgs {
          inherit system;
          config.allowUnfreePredicate = package: lib.getName package == "tart";
        };
      releaseLines = lib.filter (line: line != "" && !lib.hasPrefix "#" line) (
        lib.splitString "\n" (builtins.readFile ./versions/tart.env)
      );
      release = builtins.listToAttrs (
        map (
          line:
          let
            parsed = builtins.match "([^=]+)=(.*)" line;
          in
          {
            name = builtins.elemAt parsed 0;
            value = builtins.elemAt parsed 1;
          }
        ) releaseLines
      );
      tartFor =
        system:
        let
          pkgs = pkgsFor system;
        in
        pkgs.stdenvNoCC.mkDerivation {
          pname = "tart";
          version = release.TART_VERSION;
          src = pkgs.fetchurl {
            url = release.TART_ARCHIVE_URL;
            hash = release.TART_ARCHIVE_NIX_SHA256;
          };
          sourceRoot = ".";
          installPhase = ''
            runHook preInstall
            mkdir -p "$out/Applications"
            cp -R tart.app "$out/Applications/tart.app"
            install -Dm444 LICENSE "$out/share/tart/LICENSE"
            runHook postInstall
          '';
          # Never expose a wrapper that executes this notarized app from the Nix
          # store: macOS can add a protected com.apple.macl attribute and make the
          # output impossible for Nix to collect. GremVM copies the bundle to its
          # private runtime before verifying or executing it.
          doInstallCheck = false;
          meta = {
            description = "macOS and Linux VMs on Apple Silicon";
            homepage = "https://tart.run";
            license = lib.licenses.fsl11Asl20;
            platforms = lib.platforms.darwin;
            sourceProvenance = [ lib.sourceTypes.binaryNativeCode ];
          };
        };
    in
    {
      packages = forAllSystems (
        system:
        let
          pkgs = pkgsFor system;
          tart = tartFor system;
        in
        {
          inherit tart;
          default = pkgs.writeShellApplication {
            name = "gremvm";
            text = ''
              export GREMVM_BUNDLED_TART_APP=${tart}/Applications/tart.app
              export GREMVM_BUNDLED_TART_LICENSE=${tart}/share/tart/LICENSE
              exec ${self}/bin/gremvm "$@"
            '';
          };
        }
      );

      devShells = forAllSystems (
        system:
        let
          pkgs = pkgsFor system;
          tart = tartFor system;
        in
        {
          default = pkgs.mkShellNoCC {
            packages = [
              pkgs.bats
              pkgs.nixfmt
              pkgs.shellcheck
              pkgs.shfmt
            ];
            shellHook = ''
              export GREMVM_BUNDLED_TART_APP=${tart}/Applications/tart.app
              export GREMVM_BUNDLED_TART_LICENSE=${tart}/share/tart/LICENSE
            '';
          };
        }
      );

      formatter = forAllSystems (system: (pkgsFor system).nixfmt);

      checks = forAllSystems (
        system:
        let
          pkgs = pkgsFor system;
          tart = tartFor system;
          gremvm = self.packages.${system}.default;
        in
        {
          packaging = pkgs.runCommand "gremvm-packaging-check" { } ''
            app=${tart}/Applications/tart.app
            test -x "$app/Contents/MacOS/tart"
            test -f "$app/Contents/embedded.provisionprofile"
            test -f ${tart}/share/tart/LICENSE
            test -x ${gremvm}/bin/gremvm

            test "$(/usr/bin/plutil -extract CFBundleShortVersionString raw -o - "$app/Contents/Info.plist")" = ${release.TART_VERSION}
            test "$(/usr/bin/plutil -extract CFBundleIdentifier raw -o - "$app/Contents/Info.plist")" = ${release.TART_BUNDLE_ID}
            signature=$(/usr/bin/codesign -dv --verbose=4 "$app" 2>&1)
            printf '%s\n' "$signature" | grep -Fx 'Identifier=${release.TART_BUNDLE_ID}' >/dev/null
            printf '%s\n' "$signature" | grep -Fx 'TeamIdentifier=${release.TART_TEAM_ID}' >/dev/null
            printf '%s\n' "$signature" | grep -Eq '^CodeDirectory .*flags=.*\(runtime\)'

            ${gremvm}/bin/gremvm --help >/dev/null
            touch "$out"
          '';

          static =
            pkgs.runCommand "gremvm-static-checks"
              {
                src = self;
                nativeBuildInputs = [
                  pkgs.bats
                  pkgs.nixfmt
                  pkgs.shellcheck
                  pkgs.shfmt
                ];
              }
              ''
                cp -R "$src" source
                chmod -R u+w source
                cd source
                shellcheck bin/gremvm scripts/*.sh tests/*.sh
                shfmt -d -i 4 -ci -sr bin/gremvm scripts tests
                nixfmt --check flake.nix
                bats tests
                sh tests/smoke.sh
                touch "$out"
              '';
        }
      );
    };
}

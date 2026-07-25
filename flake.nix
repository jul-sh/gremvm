{
  description = "Pinned tooling for a Lume-managed persistent macOS VM";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-26.05-darwin";
    keytap.url = "github:jul-sh/keytap/9e1fc2930df7f6810ce2ca347822195cee0785d9";
  };

  outputs =
    {
      self,
      nixpkgs,
      keytap,
    }:
    let
      systems = [ "aarch64-darwin" ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
      pkgsFor = system: nixpkgs.legacyPackages.${system};
      vncdotoolFor =
        system:
        let
          pkgs = pkgsFor system;
        in
        pkgs.python3Packages.buildPythonApplication rec {
          pname = "vncdotool";
          version = "1.3.0";
          pyproject = true;
          src = pkgs.fetchPypi {
            inherit pname version;
            hash = "sha256-Y9ObPp0JdN96937Zcc8UE4M3GlqczFIyu3rSXDMpitY=";
          };
          build-system = [ pkgs.python3Packages.setuptools ];
          dependencies = with pkgs.python3Packages; [
            cryptography
            pillow
            twisted
          ];
          doCheck = false;
        };
    in
    {
      packages = forAllSystems (
        system:
        let
          pkgs = pkgsFor system;
        in
        {
          vncdotool = vncdotoolFor system;
          restic = pkgs.restic;
          keytap = keytap.packages.${system}.default;
          default = pkgs.writeShellApplication {
            name = "gremvm";
            runtimeInputs = [ ];
            text = ''
              exec ${self}/bin/gremvm "$@"
            '';
          };
        }
      );

      devShells = forAllSystems (
        system:
        let
          pkgs = pkgsFor system;
        in
        {
          default = pkgs.mkShellNoCC {
            packages = [
              pkgs.bats
              pkgs.nixfmt
              pkgs.restic
              pkgs.shellcheck
              pkgs.shfmt
              (vncdotoolFor system)
              keytap.packages.${system}.default
            ];
          };
        }
      );

      formatter = forAllSystems (system: (pkgsFor system).nixfmt);

      checks = forAllSystems (
        system:
        let
          pkgs = pkgsFor system;
        in
        {
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
                touch "$out"
              '';
        }
      );
    };
}

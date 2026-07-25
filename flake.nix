{
  description = "Pinned tooling for a Tart-managed persistent macOS VM";

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
    in
    {
      packages = forAllSystems (
        system:
        let
          pkgs = pkgsFor system;
        in
        {
          restic = pkgs.restic;
          keytap = keytap.packages.${system}.default;
          default = pkgs.writeShellApplication {
            name = "gremvm";
            runtimeInputs = [ ];
            text = ''
              export GREMVM_BUNDLED_CLOUDFLARED=${pkgs.cloudflared}/bin/cloudflared
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
              pkgs.cloudflared
              pkgs.curl
              pkgs.jq
              pkgs.nixfmt
              pkgs.openssl
              pkgs.restic
              pkgs.shellcheck
              pkgs.shfmt
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
                  pkgs.jq
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

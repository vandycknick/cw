{
  description = "Nix flake to build and develop cw.";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils, ... }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ ];
        };
        cargoToml = builtins.fromTOML (builtins.readFile ./Cargo.toml);
        pname = cargoToml.package.name;
        version = cargoToml.package.version or "0.1.0";
        rustPlatform = pkgs.rustPlatform;
      in {
        packages.default = rustPlatform.buildRustPackage {
          pname = pname;
          version = version;
          src = ./.;
          cargoLock.lockFile = ./Cargo.lock;
        };

        apps = {
          default = {
            type = "app";
            program = "${self.packages.${system}.default}/bin/${pname}";
          };
        };

        devShells.default = pkgs.mkShell {
          pname = "dev-shell";
          buildInputs = [ pkgs.cargo pkgs.rustc pkgs.rust-analyzer ];

          shellHook = ''
            echo "Entering dev shell — cargo, rustc and rust-analyzer are available."
          '';
        };

        defaultPackage = self.packages.${system}.default;
        defaultApp = self.apps.default;
      });
}

{
  description = "aw — find an Argo CD app and act on it";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = {
    self,
    nixpkgs,
    flake-utils,
  }:
    flake-utils.lib.eachDefaultSystem (system: let
      pkgs = import nixpkgs {inherit system;};
      aw = pkgs.rustPlatform.buildRustPackage {
        pname = "aw";
        version = "0.1.0";
        src = ./.;
        cargoLock.lockFile = ./Cargo.lock;
        meta.mainProgram = "aw";
      };
    in {
      packages.default = aw;
      packages.aw = aw;

      apps.default = {
        type = "app";
        program = "${aw}/bin/aw";
      };

      devShells.default = pkgs.mkShell {
        packages = with pkgs; [
          rustc
          cargo
          rust-analyzer
          clippy
          rustfmt
          argocd
        ];
      };
    });
}

{
  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";
    crane.url = "github:ipetkov/crane";
    crane.inputs.nixpkgs.follows = "nixpkgs";
    flake-utils.url = "github:numtide/flake-utils";
  };
  outputs = {
    self,
    nixpkgs,
    crane,
    flake-utils,
  }:
    flake-utils.lib.eachDefaultSystem (system: let
      pkgs = nixpkgs.legacyPackages.${system};
      craneLib = crane.lib.${system};
    in {
      packages.default = craneLib.buildPackage {
        src = craneLib.cleanCargoSource ./.;
      };
      devShell = pkgs.mkShell {
        packages = with pkgs;
          [cargo clippy gdb rust-analyzer rustc rustfmt]
          ++ (lib.optional (stdenv.isLinux && stdenv.isx86_64) rr);
      };
    });
}

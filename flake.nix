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
      # Mac gcc and clang don't include iconv by default, needed for building Rust bits
      iconv = with pkgs; (lib.optional stdenv.isDarwin libiconv);
    in {
      packages.default = craneLib.buildPackage {
        src = craneLib.cleanCargoSource ./.;
        buildInputs = iconv;
      };
      devShell = pkgs.mkShell {
        packages = with pkgs;
          [cargo clippy gdb rust-analyzer rustc rustfmt]
          ++ (lib.optional (stdenv.isLinux && stdenv.isx86_64) rr)
          ++ iconv;
      };
    });
}

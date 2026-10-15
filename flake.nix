{
  description = "Velk development shell";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";

  outputs =
    { nixpkgs, ... }:
    let
      systems = [
        "aarch64-darwin"
        "x86_64-darwin"
        "aarch64-linux"
        "x86_64-linux"
      ];
      forAll = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      # Rust comes from rustup via rust-toolchain.toml. NoCC keeps Nix's
      # compiler wrappers and SDK variables out of rustup's builds.
      devShells = forAll (pkgs: {
        default = pkgs.mkShellNoCC {
          packages = [
            # ghostty-vt-sys (terminal plugin) builds libghostty-vt with Zig 0.16.
            pkgs.zig_0_16
            pkgs.prek
          ];
        };
      });
    };
}

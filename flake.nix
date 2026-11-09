{
  description = "Rust development environment";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    { nixpkgs, rust-overlay, ... }:
    let
      # The Rust that `rust-version` in Cargo.toml declares, at its newest patch release, so every
      # build runs the oldest Rust the crates claim to support. A version newer than the locked
      # rust-overlay knows needs `nix flake update rust-overlay`.
      rustVersion = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).workspace.package.rust-version;

      systems = [
        "aarch64-darwin"
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems =
        f:
        nixpkgs.lib.genAttrs systems (
          system:
          f (
            import nixpkgs {
              inherit system;
              overlays = [ rust-overlay.overlays.default ];
            }
          )
        );
    in
    {
      # `nix fmt` formats every `.nix` file Git tracks; `nix fmt -- --ci` fails on one that needs it.
      formatter = forAllSystems (pkgs: pkgs.nixfmt-tree);

      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          packages = [
            (pkgs.rust-bin.fromRustupToolchain {
              channel = rustVersion;
              components = [
                "rust-src"
                "rust-analyzer"
                "clippy"
                "rustfmt"
              ];
            })

            pkgs.cargo-audit
            pkgs.cargo-deny
            pkgs.cargo-outdated
            pkgs.cargo-expand
            pkgs.cargo-flamegraph
            pkgs.git-cliff # Changelog generator

            # GitHub Actions: actionlint runs shellcheck on every `run:` block.
            pkgs.actionlint
            pkgs.shellcheck
            pkgs.zizmor # Security audit of the workflows

            # The same formatter `nix fmt` runs, at this lock's version.
            pkgs.nixfmt
          ];

          env.RUST_BACKTRACE = "1";
        };
      });
    };
}

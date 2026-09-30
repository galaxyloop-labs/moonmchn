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
      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          packages = [
            (pkgs.rust-bin.stable.latest.default.override {
              extensions = [
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
          ];

          env.RUST_BACKTRACE = "1";

          shellHook = ''
            # Daily `cargo clean`.
            # Marker lives under .git/ so it survives the clean itself;
            # `find -mtime +0` is the portable (BSD/GNU) way to test "older
            # than 24h" without juggling `stat` flag differences.
            # Cargo.toml: `code --init` loads this shell before `cargo init` runs.
            if [ -d "$PWD/.git" ] && [ -f "$PWD/Cargo.toml" ]; then
              _cargo_clean_marker="$PWD/.git/.cargo-clean-daily"
              # `code` opens three panes at once and each one evaluates this
              # hook. mkdir is atomic: one pane takes the lock, the rest skip.
              # A lock older than a day was left by an interrupted clean.
              find "$_cargo_clean_marker.lock" -maxdepth 0 -mtime +0 -exec rmdir {} \; 2>/dev/null
              if mkdir "$_cargo_clean_marker.lock" 2>/dev/null; then
                if [ ! -f "$_cargo_clean_marker" ] || [ -n "$(find "$_cargo_clean_marker" -mtime +0 2>/dev/null)" ]; then
                  echo "Running daily 'cargo clean' (target/ hasn't been reset in >24h)..."
                  cargo clean
                  touch "$_cargo_clean_marker"
                fi
                rmdir "$_cargo_clean_marker.lock"
              fi
              unset _cargo_clean_marker
            fi
          '';
        };
      });
    };
}

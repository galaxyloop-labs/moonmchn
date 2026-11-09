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
      workspacePackage = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).workspace.package;
      rustVersion = workspacePackage.rust-version;

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

      # `nix build .#mchn` builds the `mchn` binary with the same Rust as the dev shell, for the
      # release workflow to ship. A binary that runs on any host needs a build that keeps /nix/store
      # out of it:
      # - On Linux it is a static musl binary, linked by musl's own `cc`; a glibc build would point
      #   at the Nix glibc.
      # - On macOS it links the system's libiconv in place of Nix's copy.
      packages = forAllSystems (
        pkgs:
        let
          inherit (pkgs) lib;
          pname = "mchn";
          src = lib.fileset.toSource {
            root = ./.;
            fileset = lib.fileset.unions [
              ./Cargo.toml
              ./Cargo.lock
              ./LICENSE
              ./README.md
              ./crates
            ];
          };
          cargoDeps = pkgs.rustPlatform.importCargoLock { lockFile = ./Cargo.lock; };
          cargoArgs = "--release --locked --offline --package moonmchn-cli";

          linux =
            let
              musl = pkgs.pkgsStatic.stdenv;
              triple = musl.hostPlatform.rust.rustcTarget;
              cargoTarget = musl.hostPlatform.rust.cargoEnvVarTarget;
              cc = "${musl.cc}/bin/${musl.cc.targetPrefix}cc";
              toolchain = pkgs.rust-bin.fromRustupToolchain {
                channel = rustVersion;
                targets = [ triple ];
              };
            in
            pkgs.stdenv.mkDerivation {
              inherit pname src cargoDeps;
              inherit (workspacePackage) version;
              nativeBuildInputs = [
                toolchain
                pkgs.rustPlatform.cargoSetupHook
              ];
              env = {
                "CARGO_TARGET_${cargoTarget}_LINKER" = cc;
                "CC_${lib.replaceStrings [ "-" ] [ "_" ] triple}" = cc;
              };
              buildPhase = "cargo build ${cargoArgs} --target ${triple}";
              installPhase = "install -D target/${triple}/release/mchn $out/bin/mchn";
              # The release profile already strips.
              dontStrip = true;
              meta.mainProgram = "mchn";
            };

          darwin = pkgs.rustPlatform.buildRustPackage {
            inherit pname src cargoDeps;
            inherit (workspacePackage) version;
            cargoBuildFlags = [
              "--package"
              "moonmchn-cli"
            ];
            nativeBuildInputs = [ pkgs.darwin.sigtool ];
            # `make ci` runs the tests; the daemon tests need a host that Nix's build sandbox lacks.
            doCheck = false;
            # Rewriting an install name voids the signature, which Apple Silicon requires.
            postFixup = ''
              for dylib in $(otool -L "$out/bin/mchn" | awk '/\/nix\/store\/.*libiconv/ { print $1 }'); do
                install_name_tool -change "$dylib" /usr/lib/libiconv.2.dylib "$out/bin/mchn"
              done
              codesign --force --sign - "$out/bin/mchn"
            '';
            meta.mainProgram = "mchn";
          };
        in
        rec {
          mchn = if pkgs.stdenv.hostPlatform.isLinux then linux else darwin;
          default = mchn;
        }
      );

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

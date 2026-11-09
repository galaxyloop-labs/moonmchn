{
  description = "Moon MCHN official images";

  # The newest nixpkgs revision tried. Each image directory builds from the revision in its own
  # `nixpkgs.json`, which the bump moves to this one once the image builds and passes its checks.
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";

  outputs =
    { nixpkgs, ... }:
    let
      inherit (nixpkgs) lib;

      # The platforms every image is published for.
      systems = [
        "aarch64-linux"
        "x86_64-linux"
      ];

      images = import ./lib { inherit nixpkgs; };
      naming = import ./lib/naming.nix;

      # Every directory directly under `images/` is a base, but the reserved ones.
      bases = lib.filterAttrs (name: type: type == "directory" && !(lib.elem name naming.reserved)) (
        builtins.readDir ./.
      );
    in
    {
      packages = lib.genAttrs systems (system: lib.mapAttrs (name: _: images.build name system) bases);

      # What CI publishes: each output's directory, repository, moving tag and aliases.
      lib.images = lib.mapAttrs (name: _: naming.base name // { directory = name; }) bases;
    };
}

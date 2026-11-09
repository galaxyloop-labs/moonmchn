# What every image directory is built with: its own nixpkgs revision, the guest contract, and the
# name it is published under.
{ nixpkgs }:
let
  revision = import ./revision.nix { inherit nixpkgs; };
  naming = import ./naming.nix;
in
{
  # Builds the base in `images/<name>/` for `system`. Its `default.nix` receives the package set
  # of its pinned revision, the guest contract, the pin itself, and its published name.
  build =
    name: system:
    let
      dir = ./.. + "/${name}";
      pkgs = revision.packages { inherit dir system; };
    in
    import dir {
      inherit pkgs;
      guest = import ./guest.nix { inherit pkgs; };
      pin = revision.pin dir;
      image = naming.base name;
    };
}

# Each image directory builds from the nixpkgs revision in its own `nixpkgs.json`: the last one
# it built and passed its checks with. A directory that fails on a newer revision stays on its
# own while the others move, so one broken build never holds back another image's release.
{ nixpkgs }:
let
  # The file, in every image directory, that names its revision.
  pinFile = "nixpkgs.json";
in
rec {
  # The `{ rev, narHash }` that `dir` last built with.
  pin = dir: builtins.fromJSON (builtins.readFile (dir + "/${pinFile}"));

  # The package set of `dir`'s revision for `system`. A directory on the lock's revision reuses
  # the flake input; one held behind fetches its own, verified by its `narHash`.
  packages =
    { dir, system }:
    let
      pinned = pin dir;

      source =
        if pinned.rev != nixpkgs.rev then
          builtins.fetchTree {
            type = "github";
            owner = "NixOS";
            repo = "nixpkgs";
            inherit (pinned) rev narHash;
          }
        else if pinned.narHash != nixpkgs.narHash then
          throw "${toString dir}/${pinFile}: narHash ${pinned.narHash} differs from flake.lock's ${nixpkgs.narHash} for the same rev ${pinned.rev}"
        else
          nixpkgs;
    in
    import source {
      inherit system;
      config = { };
      overlays = [ ];
    };
}

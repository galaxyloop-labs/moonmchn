# Moon MCHN's default machine image
{
  pkgs,
  guest,
  pin,
  image,
}:
let
  # The CA bundle `cacert` installs, which Nix needs to fetch over TLS.
  caBundle = "/etc/ssl/certs/ca-bundle.crt";

  # `nix profile add` links its packages here, so they reach `PATH`: `mchn exec` starts no
  # login shell, so the image config is the one place `PATH` is set.
  profileBin = "${guest.home}/.nix-profile/bin";

  # The binary cache one-off installs substitute from, and its public key.
  cache = "https://cache.nixos.org";
  cacheKey = "cache.nixos.org-1:6NCHdD59X431o0gWypbMrAURkbJ16ZPMQFGspcDShjY=";

  nixConf = pkgs.writeTextDir "etc/nix/nix.conf" ''
    experimental-features = nix-command flakes
    # Single-user Nix: the store belongs to ${guest.user}, and no build users exist.
    build-users-group =
    substituters = ${cache}
    trusted-public-keys = ${cacheKey}
    # /etc/nix/registry.json pins nixpkgs; the global registry would need a download.
    flake-registry =
  '';

  # `nixpkgs#<package>` resolves to this image's own revision, so a one-off install reuses what
  # the image holds, and two machines from the same image get the same package.
  registry = pkgs.writeTextDir "etc/nix/registry.json" (
    builtins.toJSON {
      version = 2;
      flakes = [
        {
          from = {
            type = "indirect";
            id = "nixpkgs";
          };
          to = {
            type = "github";
            owner = "NixOS";
            repo = "nixpkgs";
            inherit (pin) rev narHash;
          };
        }
      ];
    }
  );

  # `#!/usr/bin/env` scripts find their interpreter on `PATH`.
  usrBinEnv = pkgs.runCommand "moonmchn-usr-bin-env" { } ''
    mkdir -p $out/usr/bin
    ln -s ${pkgs.busybox}/bin/env $out/usr/bin/env
  '';

  root = pkgs.buildEnv {
    name = "moonmchn-default-root";
    paths = [
      pkgs.busybox
      pkgs.nix
      pkgs.cacert
      nixConf
      registry
      usrBinEnv
    ];
    pathsToLink = [
      "/bin"
      "/etc"
      "/usr"
    ];
  };

  # The store paths the image holds: everything `root` refers to, `root` included.
  closure = pkgs.closureInfo { rootPaths = [ root ]; };
in
pkgs.dockerTools.buildLayeredImage {
  name = image.repository;
  inherit (image) tag;

  contents = [ root ];

  # smolvm unpacks a registry image one layer at a time and skips every entry whose path starts
  # with `/`, which is how `buildLayeredImage` names the files of its store layers. So the image
  # has none: `fakeRootCommands` copies the store into the last layer, whose files are named from
  # `./`. The smoke test's "every layer names its files relatively" holds it.
  includeStorePaths = false;
  includeNixDB = true;

  # The store and its database belong to the contract user, as a single-user install's do, so
  # its Nix can add packages without root.
  fakeRootCommands = guest.fakeRootCommands + ''
    mkdir -p ./nix/store
    xargs cp -a -t ./nix/store < ${closure}/store-paths
    chown -R -h ${toString guest.uid}:${toString guest.gid} ./nix
  '';

  config = {
    User = guest.user;
    WorkingDir = guest.workingDir;
    Env = [
      "PATH=${profileBin}:/bin"
      "NIX_SSL_CERT_FILE=${caBundle}"
    ];
    Labels = {
      "org.opencontainers.image.source" = "https://github.com/galaxyloop-labs/moonmchn";
      "org.opencontainers.image.licenses" = "Apache-2.0";
      "org.opencontainers.image.description" = "Moon MCHN's default machine";
    };
  };
}

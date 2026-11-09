# The guest contract (SPEC.md, Guest contract) that every Moon MCHN base image keeps: the
# `moonmchn` user with no password and no privileged group, its home, and a shared `/tmp`.
{ pkgs }:
let
  user = "moonmchn";
  uid = 1000;
  gid = 1000;
  home = "/home/${user}";
  shell = "/bin/sh";

  # The password field of an account that has none: no password logs in.
  noPassword = "!";

  # Modes: account files readable by all, the shadow file by root alone, root's home private,
  # and the sticky, world-writable shared temporary directory.
  accountFileMode = "0644";
  shadowMode = "0600";
  rootHomeMode = "0700";
  tmpMode = "1777";

  # dockerTools' `fakeNss` holds `root` and `nobody` alone, so the accounts are written here,
  # with `root` locked too.
  passwd = pkgs.writeText "passwd" ''
    root:x:0:0:root:/root:${shell}
    ${user}:x:${toString uid}:${toString gid}::${home}:${shell}
  '';
  group = pkgs.writeText "group" ''
    root:x:0:
    ${user}:x:${toString gid}:
  '';
  shadow = pkgs.writeText "shadow" ''
    root:${noPassword}:1::::::
    ${user}:${noPassword}:1::::::
  '';
  nsswitch = pkgs.writeText "nsswitch.conf" ''
    passwd: files
    group: files
    shadow: files
    hosts: files dns
  '';
in
{
  inherit
    user
    uid
    gid
    home
    ;

  # Shell commands for `fakeRootCommands`: the account files and the directories the contract
  # needs, with owners and modes that only a root, faked here, can set. The account files are
  # copied in as regular files: the engine reads `/etc/passwd` from outside the guest's root,
  # where a symlink into `/nix/store` resolves to nothing.
  fakeRootCommands = ''
    mkdir -p ./etc .${home} ./root ./tmp
    install -m ${accountFileMode} ${passwd} ./etc/passwd
    install -m ${accountFileMode} ${group} ./etc/group
    install -m ${accountFileMode} ${nsswitch} ./etc/nsswitch.conf
    install -m ${shadowMode} ${shadow} ./etc/shadow
    chown ${toString uid}:${toString gid} .${home}
    chmod ${rootHomeMode} ./root
    chmod ${tmpMode} ./tmp
  '';
}

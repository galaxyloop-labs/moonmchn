#!/usr/bin/env bash
# Checks that a base image archive keeps the guest contract (SPEC.md, Guest contract) and that its
# Nix works, by running it in Docker, and that every layer unpacks under the engine.
#
# Usage: images-smoke-test.sh <docker-archive>
set -euo pipefail

archive=$1

image=$(docker load --input "$archive" | sed -n 's/^Loaded image: //p')
if [[ -z $image ]]; then
  echo "FAIL docker load named no image for $archive" >&2
  exit 1
fi
echo "Checking $image"

failures=0

# Runs a command and compares its output, stderr included, with what is expected.
expect() {
  local name=$1 expected=$2
  shift 2
  local actual
  actual=$("$@" 2>&1) || true
  if [[ $actual == "$expected" ]]; then
    echo "ok   $name"
  else
    echo "FAIL $name"
    echo "     expected: $expected"
    echo "     got:      $actual"
    failures=$((failures + 1))
  fi
}

as_user() { docker run --rm "$image" "$@"; }
as_root() { docker run --rm --user 0 "$image" "$@"; }
config() { docker image inspect --format "$1" "$image"; }

# Counts the entries, across every layer of the archive, whose path starts with `/`. smolvm
# unpacks a registry image layer by layer and skips each such entry with a warning; Docker strips
# the `/` and keeps it, so no check run in Docker sees the loss.
absolute_entries() {
  tar -xOf "$archive" manifest.json | jq -r '.[0].Layers[]' | while read -r layer; do
    tar -xOf "$archive" "$layer" | tar -tP
  done | grep -c '^/'
}

expect "the contract user" "uid=1000(moonmchn) gid=1000(moonmchn) groups=1000(moonmchn)" as_user id moonmchn
expect "the default user is moonmchn" "moonmchn" as_user id -un
expect "moonmchn has no password" "!" as_root sh -c "grep '^moonmchn:' /etc/shadow | cut -d: -f2"
expect "the shadow file is root's alone" "0 600" as_user stat -c "%u %a" /etc/shadow
# The engine reads the account files from outside the guest's root, where a symlink into
# /nix/store resolves to nothing; Docker resolves it inside, so check the file type itself.
expect "the account files are regular files" "regular file regular file regular file regular file" \
  as_user sh -c "stat -c %F /etc/passwd /etc/group /etc/shadow /etc/nsswitch.conf | tr '\n' ' ' | sed 's/ \$//'"
expect "no sudo, no doas" "none" as_user sh -c "command -v sudo || command -v doas || echo none"
expect "sh, printf and tee for the announcement" "announced" as_user sh -c "printf announced | tee /dev/null"
expect "/usr/bin/env runs" "ok" as_user /usr/bin/env sh -c "echo ok"
expect "the store and the home belong to moonmchn" "1000 1000 1000" \
  as_user sh -c "stat -c %u /nix/store /nix/var/nix/db /home/moonmchn | tr '\n' ' ' | sed 's/ \$//'"
expect "a shared /tmp" "1777" as_user stat -c %a /tmp
expect "no entrypoint and no cmd" "null null" config "{{json .Config.Entrypoint}} {{json .Config.Cmd}}"
expect "every layer names its files relatively" "0" absolute_entries
# shellcheck disable=SC2016 # $PATH expands inside the image, from its config.
expect "one-off installs reach PATH" "/home/moonmchn/.nix-profile/bin:/bin" as_user sh -c 'echo "$PATH"'
expect "a one-off install works" "Hello, world!" \
  as_user sh -c "nix profile add nixpkgs#hello >/dev/null 2>&1 && hello"

if ((failures > 0)); then
  echo "$failures check(s) failed for $image" >&2
  exit 1
fi

#!/usr/bin/env bash
# Reads and moves the per-directory nixpkgs revisions against `images/flake.lock`.
#
# Usage:
#   images-revisions.sh stale            JSON array of the images not on the lock's revision
#   images-revisions.sh pin <name>       moves <name>'s nixpkgs.json to the lock's revision
#   images-revisions.sh held             rewrites the README's held list from the revisions
set -euo pipefail

lock=images/flake.lock
readme=images/README.md
pin_file=nixpkgs.json
held_heading="### Held directories"

lock_rev() { jq -r '.nodes.nixpkgs.locked.rev' "$lock"; }
lock_nar_hash() { jq -r '.nodes.nixpkgs.locked.narHash' "$lock"; }

# Every image as `<name> <directory>` lines.
images() {
  nix eval --json ./images#lib.images | jq -r 'to_entries[] | "\(.key) \(.value.directory)"'
}

directory_of() {
  nix eval --raw "./images#lib.images.$1.directory"
}

pinned_rev() { jq -r '.rev' "images/$1/$pin_file"; }

stale() {
  local rev name directory
  rev=$(lock_rev)
  images | while read -r name directory; do
    if [[ $(pinned_rev "$directory") != "$rev" ]]; then echo "$name"; fi
  done | jq -R . | jq -cs .
}

pin() {
  local directory
  directory=$(directory_of "$1")
  jq -n --arg rev "$(lock_rev)" --arg narHash "$(lock_nar_hash)" '{rev: $rev, narHash: $narHash}' \
    >"images/$directory/$pin_file"
}

# The held list: every image whose revision is behind the lock's, with the date its revision
# last moved, so a held image stays visible until it builds again.
held_lines() {
  local rev name directory pinned since
  rev=$(lock_rev)
  images | while read -r name directory; do
    pinned=$(pinned_rev "$directory")
    if [[ $pinned != "$rev" ]]; then
      since=$(git log -1 --format=%cs -- "images/$directory/$pin_file")
      echo "- \`images/$directory/\`: nixpkgs \`${pinned:0:12}\`, since ${since:-today}."
    fi
  done
}

held() {
  local lines
  lines=$(held_lines)
  lines=${lines:-None.}
  # Replaces the body between the held heading and the next heading. The body goes through the
  # environment, since awk's -v rejects newlines on some platforms.
  HELD_HEADING=$held_heading HELD_BODY=$lines awk '
    $0 == ENVIRON["HELD_HEADING"] { print; print ""; print ENVIRON["HELD_BODY"]; print ""; skipping = 1; next }
    skipping && /^#/ { skipping = 0 }
    !skipping { print }
  ' "$readme" >"$readme.new"
  mv "$readme.new" "$readme"
}

case ${1:-} in
  stale) stale ;;
  pin) pin "$2" ;;
  held) held ;;
  *) echo "usage: $0 stale | pin <name> | held" >&2; exit 2 ;;
esac

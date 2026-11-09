#!/usr/bin/env bash
# Prints, as a JSON array, the name of every image the images flake builds.
#
# Usage: images-names.sh
set -euo pipefail

nix eval --json ./images#lib.images --apply builtins.attrNames

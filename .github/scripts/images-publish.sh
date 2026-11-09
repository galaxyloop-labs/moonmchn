#!/usr/bin/env bash
# Publishes one image, given one archive per platform, as an index under an immutable tag, signs
# it, then moves its moving tag and aliases to it. An image whose content is already what the
# moving tag holds is left alone: two builds of the same inputs differ in nothing a user sees, and
# republishing would only churn digests that machine specs pin.
#
# Usage: images-publish.sh --repository <repo> --tag <tag> --immutable <tag> [--alias <tag>]...
#                          [--sign] [--insecure] <archive>...
#
# --sign signs the index and each platform's manifest with cosign, keyless. --insecure talks plain
# HTTP to the registry, for a local test registry alone. With GITHUB_OUTPUT set, writes
# `digest=<index digest>` to it when something was published.
set -euo pipefail

repository="" tag="" immutable="" sign=false insecure=false
aliases=()
while (($# > 0)); do
  case $1 in
    --repository) repository=$2; shift 2 ;;
    --tag) tag=$2; shift 2 ;;
    --immutable) immutable=$2; shift 2 ;;
    --alias) aliases+=("$2"); shift 2 ;;
    --sign) sign=true; shift ;;
    --insecure) insecure=true; shift ;;
    --) shift; break ;;
    -*) echo "unknown option: $1" >&2; exit 2 ;;
    *) break ;;
  esac
done
archives=("$@")
if [[ -z $repository || -z $tag || -z $immutable || ${#archives[@]} -eq 0 ]]; then
  echo "usage: $0 --repository <repo> --tag <tag> --immutable <tag> [--alias <tag>]... [--sign] [--insecure] <archive>..." >&2
  exit 2
fi

crane_flags=()
skopeo_flags=()
if $insecure; then
  crane_flags=(--insecure)
  skopeo_flags=(--dest-tls-verify=false)
fi

# The digest of an archive's image config: the image's identity, whatever the compression of the
# layers a push produces.
archive_config_digest() {
  local config
  config=$(tar -xzOf "$1" manifest.json | jq -r '.[0].Config')
  echo "sha256:${config%.json}"
}

archive_architecture() {
  local config
  config=$(tar -xzOf "$1" manifest.json | jq -r '.[0].Config')
  tar -xzOf "$1" "$config" | jq -r '.architecture'
}

# The config digests of the platforms the moving tag points at today, none when it is unpublished.
published_config_digests() {
  local index
  index=$(crane manifest "${crane_flags[@]}" "$repository:$tag" 2>/dev/null) || return 0
  jq -r '.manifests[].digest' <<<"$index" | while read -r digest; do
    crane manifest "${crane_flags[@]}" "$repository@$digest" | jq -r '.config.digest'
  done
}

local_configs=$(for archive in "${archives[@]}"; do archive_config_digest "$archive"; done | sort)
published_configs=$(published_config_digests | sort)
if [[ $local_configs == "$published_configs" ]]; then
  echo "$repository:$tag already holds this content; nothing to publish"
  exit 0
fi

manifests=()
for archive in "${archives[@]}"; do
  architecture=$(archive_architecture "$archive")
  digest_file=$(mktemp)
  # The source is our own archive, built and smoke-tested in this run, so no signature policy
  # applies to it; without the flag, skopeo needs a policy.json some hosts lack. The archive's own
  # Docker format keeps its config bytes, and so the config digest the comparison above reads;
  # converting to OCI would rewrite the config.
  skopeo --insecure-policy copy "${skopeo_flags[@]}" --digestfile "$digest_file" \
    "docker-archive:$archive" "docker://$repository:$tag-$architecture"
  manifests+=(--manifest "$repository@$(cat "$digest_file")")
  rm -f "$digest_file"
done

# A Docker manifest list, to match the Docker manifests it holds.
crane index append "${crane_flags[@]}" --docker-empty-base "${manifests[@]}" --tag "$repository:$immutable"
index_digest=$(crane digest "${crane_flags[@]}" "$repository:$immutable")

# Signed before any moving tag points at it, so no tag ever names an unsigned index.
if $sign; then
  cosign sign --yes --recursive "$repository@$index_digest"
fi

for moving in "$tag" "${aliases[@]}"; do
  crane tag "${crane_flags[@]}" "$repository@$index_digest" "$moving"
done

echo "Published $repository@$index_digest as $immutable, $tag${aliases[*]:+ and ${aliases[*]}}"
if [[ -n ${GITHUB_OUTPUT:-} ]]; then
  echo "digest=$index_digest" >>"$GITHUB_OUTPUT"
fi

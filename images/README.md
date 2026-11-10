# Moon MCHN images

The official images Moon MCHN machines boot, built with Nix `dockerTools` from nixpkgs
unstable. Each one keeps the guest contract in [SPEC.md](../docs/SPEC.md#guest-contract).

## Kinds and names

| Directory | Kind | Published as |
|---|---|---|
| `images/<base>/` | A base image a machine boots | `ghcr.io/galaxyloop-labs/moonmchn:<base>` |
| `images/packs/<name>/` | A pack stacked onto `default` (not built yet) | `ghcr.io/galaxyloop-labs/moonmchn/packs/<name>:latest` |
| `images/services/<name>/` | A service sidekick (not built yet) | `ghcr.io/galaxyloop-labs/moonmchn/services/<name>:latest` |

- A directory directly under `images/` is a base. `lib`, `packs`, `services` and `latest` are
  reserved and never name one.
- `moonmchn:latest` points at `moonmchn:default`, so a reference without a tag boots the
  default image.
- Every published tag is an index holding `linux/arm64` and `linux/amd64`. Each build also
  gets an immutable tag, `<base>-<date>-<commit>` for a base and `<date>-<commit>` otherwise,
  so a digest a machine's spec pins keeps a name when the moving tag moves.

## Layout

```
images/
  flake.nix        # one output per image, per platform
  flake.lock       # the newest nixpkgs revision tried
  lib/             # the guest contract and the per-directory revision
  default/
    default.nix
    nixpkgs.json   # the nixpkgs revision this image last built with
```

## Revisions

- Every image directory builds from the nixpkgs revision in its own `nixpkgs.json`: the `rev`
  it last built and passed its checks with, and that revision's `narHash`.
- `flake.lock` holds the newest revision tried. A bump moves each directory's
  `nixpkgs.json` to it once the image builds and passes its checks there. A directory that
  fails stays on its own revision, so one broken build never holds back another image.
- A held directory keeps its old revision's vulnerabilities, so it is listed below until it
  builds again.

### Held directories

None.

## `default`

- The guest contract: the `moonmchn` user (uid 1000, no password), its home as `WORKDIR`,
  `/tmp`.
- BusyBox with its full set of applets in `/bin`, and `/usr/bin/env`.
- Single-user Nix owned by `moonmchn`, with `nix-command` and `flakes` on, and `nixpkgs`
  pinned to the image's own revision. A one-off install inside a machine:

  ```sh
  nix profile add nixpkgs#ripgrep
  ```

  It needs `github.com`, `codeload.github.com` and `cache.nixos.org` in `[egress] allow`,
  which `mchn setup`'s `allow = ["*"]` covers. Under the smolmachines engine, `mchn stop`
  discards the machine's disk, and the installs with it.

## Building

Building needs Nix 2.34 or later with flakes. Images are Linux images:
on macOS, Nix needs a Linux builder, such as nix-darwin's `nix.linux-builder`.

`nix fmt`, from the repository's root, formats the Nix code here with the root flake's
`nixfmt`; `make nix-fmt-check` fails on a file that needs it.

```sh
nix build ./images#packages.aarch64-linux.default   # or x86_64-linux
```

`result` is a gzipped `docker save` archive. `mchn start` takes an archive whose name ends in
`.tar.gz`, so copy it first:

```sh
cp -L result default.tar.gz
mchn start try --image "$PWD/default.tar.gz"
```

`docker load -i result` loads it into Docker for a look inside.

## Publishing

CI publishes every image, and nothing is pushed from a laptop. The checks below certify an
image; no person does.

| Workflow | Runs | Does |
|---|---|---|
| `images-check` | on pull requests touching images | `nix fmt -- --ci`; builds every image on both platforms and smoke-tests it |
| `images-publish` | on pushes to `main` touching images, or by hand | builds and smoke-tests, then signs and publishes each image on its own |
| `images-bump` | Mondays at 05:17 UTC, or by hand for a CVE | moves `flake.lock` to the newest nixpkgs-unstable, tries each image behind it, commits to `main` the revisions that passed and the held list |

- **One image at a time:** every image builds, and publishes, in jobs of its own, so one failure
  stops no other.
- **Smoke test** (`.github/scripts/images-smoke-test.sh`): the guest contract, the account
  files as regular files (the engine reads them from outside the guest's root), the store's
  owner, a one-off `nix profile add nixpkgs#hello`, and no layer entry whose path starts with
  `/` (the engine skips those when it pulls from a registry).
- **Unchanged images stay put:** an image whose platforms' configs equal those the moving tag
  holds is not republished, so the bump republishes only what moved.
- **Order:** each platform's manifest is pushed under `<tag>-<arch>`, a manifest list joins them
  under the immutable tag, cosign signs it keyless with the workflow's identity, then the moving
  tag and its aliases move to it. A provenance attestation goes with it.
- **Verify** a published image:

  ```sh
  cosign verify ghcr.io/galaxyloop-labs/moonmchn:default \
    --certificate-identity-regexp '^https://github.com/galaxyloop-labs/moonmchn/' \
    --certificate-oidc-issuer https://token.actions.githubusercontent.com
  gh attestation verify oci://ghcr.io/galaxyloop-labs/moonmchn:default --owner galaxyloop-labs
  ```

### Setup, once

- **The bump's GitHub App:** a push made with `GITHUB_TOKEN` runs no workflow, so the bump
  commits through an App, and its push runs `images-publish`.
  - Create an App with the repository permission Contents: read and write, and install it on
    this repository.
  - Store its id in the repository variable `IMAGES_BUMP_APP_ID`, its private key in the secret
    `IMAGES_BUMP_APP_PRIVATE_KEY`.
  - If `main` is protected, let the App bypass the rule, so it can push.
- **Public packages:** a new package starts private on ghcr.io. Once its first publish lands,
  make it public in its package settings (Package settings, Danger Zone, Change visibility),
  since the Moon MCHN daemon resolves images anonymously.

# Where each image directory is published. A base, a directory directly under `images/`, is a tag
# of the `moonmchn` package; packs and services, once built, get packages of their own.
let
  # The package every base is a tag of.
  registry = "ghcr.io/galaxyloop-labs/moonmchn";

  # The base a reference without a tag boots, through the alias the reference parser fills in.
  defaultBase = "default";
  latest = "latest";
in
{
  # Directories under `images/` that never name a base.
  reserved = [
    "lib"
    "packs"
    "services"
    latest
  ];

  # The repository, moving tag and aliases of the base in `images/<name>/`.
  base = name: {
    repository = registry;
    tag = name;
    aliases = if name == defaultBase then [ latest ] else [ ];
  };
}

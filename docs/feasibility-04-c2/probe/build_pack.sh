#!/bin/sh
# Assemble the experimental developer pack: the pinned SearXNG source, its
# hash-pinned wheels and the bridge worker. Developer use only: this is not a
# distributable artifact and the application never runs it.
#
#   sh build_pack.sh <new-empty-directory>
set -eu
REVISION=b5605b8f2fa2591108ab8e3f4fc16e88f7d66fe4
HERE=$(cd "$(dirname "$0")" && pwd)
PACK=${1:?usage: build_pack.sh <new-empty-directory>}
mkdir -p "$PACK"
[ -z "$(ls -A "$PACK")" ] || { echo "$PACK is not empty" >&2; exit 1; }
PACK=$(cd "$PACK" && pwd)
git init --quiet "$PACK/searxng"
git -C "$PACK/searxng" fetch --quiet --depth 1 https://github.com/searxng/searxng.git "$REVISION"
git -C "$PACK/searxng" checkout --quiet --detach FETCH_HEAD
[ "$(git -C "$PACK/searxng" rev-parse HEAD)" = "$REVISION" ]
uv pip install --quiet --python /usr/bin/python3 --require-hashes --only-binary :all: \
    --target "$PACK/site-packages" -r "$HERE/requirements.lock"
# Upstream's own mechanism for running outside a Git checkout; without it the
# import starts git, which the sandbox does not allow.
cat > "$PACK/searxng/searx/version_frozen.py" <<EOT
VERSION_STRING = "${REVISION}"
VERSION_TAG = "${REVISION}"
DOCKER_TAG = "${REVISION}"
GIT_URL = "https://github.com/searxng/searxng"
GIT_BRANCH = "master"
EOT
cp "$HERE/worker.py" "$HERE/settings.yml" "$PACK/"
echo "$PACK"

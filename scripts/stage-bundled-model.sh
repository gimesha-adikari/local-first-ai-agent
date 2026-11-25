#!/usr/bin/env bash
# Stage the LFM2.5 GGUF for packaging without copying it into Git.
# Usage: scripts/stage-bundled-model.sh /path/to/LFM2.5-1.2B-Instruct-Q4_K_M.gguf
# The file is verified against the pinned size and SHA-256, then linked into
# the ignored apps/desktop/src-tauri/resources/models directory. Nothing is
# downloaded.
set -euo pipefail

readonly NAME="LFM2.5-1.2B-Instruct-Q4_K_M.gguf"
readonly BYTES=730895168
readonly SHA256="b1b3de114215d9507409a662a501a631095a479a419584e8a2ded6304b19b4f5"

source_path="${1:?usage: $0 /path/to/$NAME}"
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
target_dir="$root/apps/desktop/src-tauri/resources/models"

if [[ ! -f "$source_path" ]]; then
  echo "error: $source_path is not a regular file" >&2
  exit 1
fi
size="$(stat -c %s -- "$source_path")"
if [[ "$size" != "$BYTES" ]]; then
  echo "error: expected $BYTES bytes, found $size" >&2
  exit 1
fi
echo "Verifying SHA-256..."
digest="$(sha256sum -- "$source_path" | cut -d' ' -f1)"
if [[ "$digest" != "$SHA256" ]]; then
  echo "error: SHA-256 mismatch ($digest)" >&2
  exit 1
fi
mkdir -p "$target_dir"
ln -sfn -- "$(realpath -- "$source_path")" "$target_dir/$NAME"
echo "Staged $target_dir/$NAME"
echo "Build: npm --prefix apps/desktop run tauri -- build --config src-tauri/tauri.bundled-model.conf.json -- --locked"

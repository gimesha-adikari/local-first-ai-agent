#!/bin/sh
# Run with two existing packages. Uses disposable dpkg roots and does not run
# package configuration or touch the host package database.
set -eu

if [ "$#" -ne 2 ]; then
    echo 'Usage: sh scripts/check-deb-model-upgrade.sh OLD_BUNDLED.deb NEW_BASE.deb' >&2
    exit 2
fi

scratch=$(mktemp -d)
trap 'rm -rf -- "$scratch"' EXIT
unpack() {
    mkdir -p -- "$1"
    fakeroot dpkg --root="$1" --log="$1/dpkg.log" --force-script-chrootless --unpack "$2"
}

model=models/LFM2.5-1.2B-Instruct-Q4_K_M.gguf
package=local-first-ai-agent
fresh=$scratch/fresh
upgrade=$scratch/upgrade
old=$upgrade/usr/lib/$package/$model
retained=$upgrade/var/lib/$package/$model

unpack "$fresh" "$2"
test -z "$(find "$fresh" -name '*.gguf' -print)"

unpack "$upgrade" "$1"
old_inode=$(stat -c '%d:%i' -- "$old")
old_mode=$(stat -c '%a' -- "$old")
old_digest=$(sha256sum < "$old")

mv -- "$old" "$old.fixture"
printf 'invalid legacy artifact\n' > "$old"
chmod 0644 "$old"
if unpack "$upgrade" "$2"; then
    echo 'The upgrade accepted an invalid legacy artifact.' >&2
    exit 1
fi
test "$(cat "$old")" = 'invalid legacy artifact'
test ! -e "$retained"
mv -f -- "$old.fixture" "$old"

unpack "$upgrade" "$2"
test ! -e "$old"
test -f "$retained"
test "$old_inode" = "$(stat -c '%d:%i' -- "$retained")"
test "$old_mode" = "$(stat -c '%a' -- "$retained")"
test "$old_digest" = "$(sha256sum < "$retained")"

unpack "$upgrade" "$2"
test "$old_inode" = "$(stat -c '%d:%i' -- "$retained")"
echo 'Clean install, failed verification, bundled-model upgrade and repeat unpack passed.'

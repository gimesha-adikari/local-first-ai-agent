#!/bin/sh
# Run the opt-in Camera portal test against synthetic cameras in a
# disposable, network-less headless GNOME session (development only).
# Usage: run_camera_test.sh <test executable> <output dir>
set -eu
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../.." && pwd)
exe=$1
out=$2
mkdir -p "$out/fixtures"
python3 -I "$here/make_fixtures.py" "$out/fixtures" >/dev/null
probe=$repo/docs/feasibility-03-e/probe
export AGENT_REPO="$repo" AGENT_CAMERA_SOURCE="$here/camera_source.sh"
export AGENT_CAMERA_FIXTURES="$out/fixtures" AGENT_CAMERA_OUT="$out" AGENT_CAMERA_FOCUS="$here/focus_window.py"
exec unshare --map-root-user --net --pid --fork --kill-child --mount-proc sh -c '
  ip link set lo up || exit
  exec sh "$1/session.sh" "$1" sh "$1/with-portal.sh" "$2" \
    camera::tests::portal_camera_snapshots_in_a_headless_session --exact --ignored --nocapture
' sh "$probe" "$exe"

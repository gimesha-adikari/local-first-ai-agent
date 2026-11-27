#!/bin/sh
# Disposable headless GNOME Shell with private PipeWire and D-Bus. Run under
# `unshare --user --map-root-user --net` so nothing can reach the network.
# Usage: session.sh <probe dir> <command...>
set -u
here=$1; shift
p=$(mktemp -d /tmp/agent-03e-capture.XXXXXX)
export HOME=$p/home XDG_CONFIG_HOME=$p/config XDG_DATA_HOME=$p/data XDG_CACHE_HOME=$p/cache XDG_STATE_HOME=$p/state XDG_RUNTIME_DIR=$p/runtime
mkdir -p "$HOME" "$XDG_CONFIG_HOME" "$XDG_DATA_HOME" "$XDG_CACHE_HOME" "$XDG_STATE_HOME" "$XDG_RUNTIME_DIR"; chmod 700 "$XDG_RUNTIME_DIR"
unset WAYLAND_DISPLAY DISPLAY DBUS_SESSION_BUS_ADDRESS GNOME_KEYRING_CONTROL
export XDG_CURRENT_DESKTOP=GNOME XDG_SESSION_TYPE=wayland XDG_SESSION_DESKTOP=gnome NO_AT_BRIDGE=1 GSETTINGS_BACKEND=keyfile
mkdir -p "$XDG_CONFIG_HOME/glib-2.0/settings"; printf "[org/gnome/desktop/interface]\nenable-hot-corners=false\n" >"$XDG_CONFIG_HOME/glib-2.0/settings/keyfile"
echo "profile $p"
cat >"$p/bus.conf" <<'CONF'
<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN" "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig><type>session</type><listen>unix:tmpdir=/tmp</listen><auth>EXTERNAL</auth>
<policy context="default"><allow send_destination="*"/><allow receive_sender="*"/><allow own="*"/></policy></busconfig>
CONF
exec dbus-run-session --config-file="$p/bus.conf" -- sh -c '
  here=$1; shift
  pipewire >"$XDG_RUNTIME_DIR/pipewire.log" 2>&1 &
  sleep 0.5
  wireplumber >"$XDG_RUNTIME_DIR/wireplumber.log" 2>&1 &
  sleep 0.5
  gnome-shell --headless --wayland --no-x11 --virtual-monitor 1280x800 --wayland-display=wayland-capture >"$XDG_RUNTIME_DIR/shell.log" 2>&1 &
  for i in $(seq 1 100); do
    gdbus introspect --session --dest org.gnome.Mutter.ScreenCast --object-path /org/gnome/Mutter/ScreenCast >/dev/null 2>&1 && break
    sleep 0.2
  done
  export WAYLAND_DISPLAY=wayland-capture
  "$@"
  status=$?
  echo "--- shell.log tail"; tail -5 "$XDG_RUNTIME_DIR/shell.log"
  exit $status
' sh "$here" "$@"

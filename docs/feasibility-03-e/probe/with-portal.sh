#!/bin/sh
# Inside session.sh: start the permission store and portals on the private bus.
# The host Registry needs the desktop file and its Exec binary on PATH.
mkdir -p "$XDG_DATA_HOME/applications"
mkdir -p "$HOME/bin"; printf '#!/bin/sh\nexit 0\n' >"$HOME/bin/local-first-ai-agent"; chmod +x "$HOME/bin/local-first-ai-agent"
export PATH="$HOME/bin:$PATH"
cp "${AGENT_REPO:?set AGENT_REPO to the repository root}/apps/desktop/src-tauri/io.github.gimesha-adikari.localfirstaiagent.desktop" "$XDG_DATA_HOME/applications/"
/usr/libexec/xdg-permission-store >"$XDG_RUNTIME_DIR/perm.log" 2>&1 &
/usr/libexec/xdg-desktop-portal-gnome >"$XDG_RUNTIME_DIR/xdpg.log" 2>&1 &
# xdg-desktop-portal only exports ScreenCast if the GNOME backend is already
# on the bus when it starts.
for i in $(seq 1 50); do gdbus introspect --session --dest org.freedesktop.impl.portal.desktop.gnome --object-path /org/freedesktop/portal/desktop >/dev/null 2>&1 && break; sleep 0.2; done
/usr/libexec/xdg-desktop-portal >"$XDG_RUNTIME_DIR/xdp.log" 2>&1 &
for i in $(seq 1 50); do gdbus introspect --session --dest org.freedesktop.portal.Desktop --object-path /org/freedesktop/portal/desktop >/dev/null 2>&1 && break; sleep 0.2; done
"$@"; s=$?
echo "--- xdp"; tail -5 "$XDG_RUNTIME_DIR/xdp.log"; echo "--- xdpg"; tail -5 "$XDG_RUNTIME_DIR/xdpg.log"
exit $s

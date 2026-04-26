#!/usr/bin/env bash
# Drive the Linux Wayland libei + RemoteDesktop portal path end-to-end
# inside the xa11y-wayland-libei container:
#
#   1. start a D-Bus session bus
#   2. start mutter with the headless backend
#   3. start pipewire + wireplumber (the GNOME portal needs them)
#   4. start xdg-desktop-portal + xdg-desktop-portal-gnome
#   5. pre-grant RemoteDesktop access via the permission store so the
#      portal doesn't block on consent UI
#   6. run xa11y-linux's `wayland_input_e2e` tests with WAYLAND_DISPLAY set
#      and DISPLAY unset, forcing the libei code path
#
# The headless-portal-consent path is the riskiest piece. If pre-grant
# fails the call to RemoteDesktop.Start will block forever, so each
# `cargo test` is wrapped with a 60s timeout.
exec 1> >(stdbuf -o0 cat) 2>&1
set -euo pipefail

if [ -z "${DBUS_SESSION_BUS_ADDRESS:-}" ]; then
    exec dbus-run-session -- bash "$0" "$@"
fi

export XDG_RUNTIME_DIR=/run/user/0
mkdir -p "$XDG_RUNTIME_DIR"
chmod 700 "$XDG_RUNTIME_DIR"

# Tell every component this is a GNOME-style Wayland session.
export NO_AT_BRIDGE=1
export XDG_SESSION_TYPE=wayland
export XDG_CURRENT_DESKTOP=GNOME

CLEANUP_PIDS=()
cleanup() {
    for pid in "${CLEANUP_PIDS[@]}"; do
        kill "$pid" 2>/dev/null || true
        wait "$pid" 2>/dev/null || true
    done
}
trap cleanup EXIT

# 1. Start mutter --headless. Mutter announces a wl_display socket and an
# EIS server; the portal-gnome backend will route RemoteDesktop sessions
# to that EIS.
mutter --headless --no-x11 --virtual-monitor 1280x1024 \
    >/tmp/mutter.log 2>&1 &
CLEANUP_PIDS+=($!)

for _ in $(seq 1 50); do
    if compgen -G "$XDG_RUNTIME_DIR/wayland-*" >/dev/null; then
        break
    fi
    sleep 0.2
done
sock=$(ls "$XDG_RUNTIME_DIR"/wayland-* 2>/dev/null | head -1 || true)
if [ -z "$sock" ]; then
    echo "mutter failed to create a Wayland socket. Log:" >&2
    cat /tmp/mutter.log >&2 || true
    exit 1
fi
export WAYLAND_DISPLAY="$(basename "$sock")"
unset DISPLAY
echo "WAYLAND_DISPLAY=$WAYLAND_DISPLAY"

# 2. pipewire + wireplumber — RemoteDesktop binds ScreenCast which needs
# a working pipewire graph even when only input is requested.
pipewire >/tmp/pipewire.log 2>&1 &
CLEANUP_PIDS+=($!)
wireplumber >/tmp/wireplumber.log 2>&1 &
CLEANUP_PIDS+=($!)
for _ in $(seq 1 25); do
    if pw-cli info 0 >/dev/null 2>&1; then
        break
    fi
    sleep 0.2
done

# 3. gnome-remote-desktop daemon — implements the RemoteDesktop backend
# that xdg-desktop-portal-gnome delegates to. Without this running the
# portal frontend logs `error: Could not connect` for the RemoteDesktop
# interface and never registers it on the bus.
GRD_BIN=""
for cand in /usr/libexec/gnome-remote-desktop-daemon \
            /usr/lib/x86_64-linux-gnu/gnome-remote-desktop-daemon \
            /usr/lib/gnome-remote-desktop/gnome-remote-desktop-daemon; do
    if [ -x "$cand" ]; then
        GRD_BIN="$cand"
        break
    fi
done
if [ -z "$GRD_BIN" ]; then
    GRD_BIN="$(command -v gnome-remote-desktop-daemon || true)"
fi
if [ -z "$GRD_BIN" ]; then
    echo "WARN: gnome-remote-desktop-daemon binary not found; portal will lack RemoteDesktop"
    dpkg -L gnome-remote-desktop 2>&1 | head -40 || true
else
    echo "Starting $GRD_BIN --headless"
    "$GRD_BIN" --headless >/tmp/grd.log 2>&1 &
    CLEANUP_PIDS+=($!)
    # Wait for the daemon to claim its bus name. Without this the
    # portal-gnome init races and skips RemoteDesktop registration.
    for _ in $(seq 1 50); do
        if dbus-send --session --print-reply --dest=org.gnome.RemoteDesktop \
             /org/gnome/RemoteDesktop \
             org.freedesktop.DBus.Introspectable.Introspect >/dev/null 2>&1; then
            echo "OK: org.gnome.RemoteDesktop is on the bus."
            break
        fi
        sleep 0.2
    done
fi

# 4. Portal frontend + GNOME backend.
mkdir -p /root/.config/xdg-desktop-portal
cat > /root/.config/xdg-desktop-portal/GNOME-portals.conf <<'EOF'
[preferred]
default=gnome
org.freedesktop.impl.portal.RemoteDesktop=gnome
org.freedesktop.impl.portal.ScreenCast=gnome
org.freedesktop.impl.portal.Access=gnome
EOF

/usr/libexec/xdg-desktop-portal-gnome >/tmp/portal-gnome.log 2>&1 &
CLEANUP_PIDS+=($!)
/usr/libexec/xdg-desktop-portal -v >/tmp/portal.log 2>&1 &
CLEANUP_PIDS+=($!)

# Give the portal time to register its objects on the bus. The wlr
# screenshot script uses 10s; libei's portal has a heavier startup so
# we give it 15.
sleep 15

# 4. Pre-grant RemoteDesktop access. xdg-desktop-portal looks up
# permissions in `~/.local/share/xdg-desktop-portal/permissions.db`
# (a sqlite file managed via the org.freedesktop.impl.portal.PermissionStore
# bus interface). We pre-create a row granting our test binary access so
# `RemoteDesktop.Start` doesn't block on consent UI in headless CI.
#
# The app-id for cargo-built binaries is empty / "" — the portal stores
# permissions per-app so we set the entry under the snap-style identifier
# the portal would compute for an unsandboxed binary.
#
# NOTE: this is best-effort. If the GNOME portal version doesn't accept
# a pre-grant via PermissionStore for RemoteDesktop, the script falls
# through and the cargo test below will time out — which the CI job
# treats as a soft fail until this path stabilises.
echo "---preseeding RemoteDesktop permission---"
gdbus call --session \
    --dest=org.freedesktop.impl.portal.PermissionStore \
    --object-path=/org/freedesktop/impl/portal/PermissionStore \
    --method=org.freedesktop.impl.portal.PermissionStore.SetPermission \
    "remote-desktop" true "remote-desktop" "" '["yes"]' 2>&1 || \
    echo "PermissionStore.SetPermission returned non-zero (may be normal)"

# 5. Confirm the RemoteDesktop interface is registered before running
# tests; print logs on failure for fast diagnosis.
echo "---RemoteDesktop interface check---"
if dbus-send --session --print-reply \
     --dest=org.freedesktop.portal.Desktop \
     /org/freedesktop/portal/desktop \
     org.freedesktop.DBus.Introspectable.Introspect 2>&1 \
     | grep -q 'org\.freedesktop\.portal\.RemoteDesktop'; then
    echo "OK: portal RemoteDesktop interface is registered."
else
    echo "WARN: portal RemoteDesktop interface not registered; test will fail."
fi

echo "---portal-gnome log---"
tail -30 /tmp/portal-gnome.log 2>/dev/null || true
echo "---portal log---"
tail -30 /tmp/portal.log 2>/dev/null || true
echo "---mutter log---"
tail -20 /tmp/mutter.log 2>/dev/null || true
echo "---end logs---"

cd /xa11y
echo "Building xa11y-linux..."
cargo build -p xa11y-linux >/tmp/build.log 2>&1 || {
    echo "build failed; tail of /tmp/build.log:"
    tail -60 /tmp/build.log
    exit 1
}

set +e
timeout 60 cargo test -p xa11y-linux --test wayland_input_e2e \
    -- --ignored --test-threads=1 --nocapture
rc=$?
set -e

echo "---portal-gnome log (final)---"
tail -40 /tmp/portal-gnome.log 2>/dev/null || true
echo "---portal log (final)---"
tail -40 /tmp/portal.log 2>/dev/null || true
echo "---mutter log (final)---"
tail -20 /tmp/mutter.log 2>/dev/null || true
echo "---end---"
exit $rc

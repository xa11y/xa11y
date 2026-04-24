#!/usr/bin/env bash
# Force unbuffered output — docker run otherwise batches diagnostic prints
# from this wrapper with the `cargo test` output at the end.
exec 1> >(stdbuf -o0 cat) 2>&1
# Drive the Linux Wayland portal-Screenshot path end-to-end inside the
# xa11y-wayland container:
#   1. start a D-Bus session bus
#   2. start sway (wlroots) with the headless backend
#   3. start xdg-desktop-portal + xdg-desktop-portal-wlr
#   4. run the xa11y-linux screenshot tests with WAYLAND_DISPLAY set and
#      DISPLAY unset, forcing the portal code path
#
# The portal-wlr backend auto-approves screenshot requests for clients it
# can identify, which is what we need in non-interactive CI. If consent is
# asked for, the call will block forever — the harness applies a 30s
# timeout to each `cargo test` to surface that case cleanly.
set -euo pipefail

if [ -z "${DBUS_SESSION_BUS_ADDRESS:-}" ]; then
    exec dbus-run-session -- bash "$0" "$@"
fi

export XDG_RUNTIME_DIR=/run/user/0
mkdir -p "$XDG_RUNTIME_DIR"
chmod 700 "$XDG_RUNTIME_DIR"

# Suppress the "can't start the accessibility bus" chatter from sway — it's
# not relevant to the screenshot path.
export NO_AT_BRIDGE=1
export XDG_SESSION_TYPE=wayland
export XDG_CURRENT_DESKTOP=sway

CLEANUP_PIDS=()
cleanup() {
    for pid in "${CLEANUP_PIDS[@]}"; do
        kill "$pid" 2>/dev/null || true
        wait "$pid" 2>/dev/null || true
    done
}
trap cleanup EXIT

# 1. Start sway with the headless wlroots backend. A minimal config is
# enough — we just need the compositor up so the wl_display socket exists.
cat > /tmp/sway.conf <<'EOF'
# Minimal headless sway config: no inputs, one virtual output.
output HEADLESS-1 resolution 1280x1024 position 0,0
EOF
WLR_BACKENDS=headless \
WLR_LIBINPUT_NO_DEVICES=1 \
WLR_RENDERER=pixman \
sway -c /tmp/sway.conf >/tmp/sway.log 2>&1 &
CLEANUP_PIDS+=($!)

# Wait for sway to create the Wayland socket.
for _ in $(seq 1 30); do
    if compgen -G "$XDG_RUNTIME_DIR/wayland-*" >/dev/null; then
        break
    fi
    sleep 0.2
done
sock=$(ls "$XDG_RUNTIME_DIR"/wayland-* 2>/dev/null | head -1 || true)
if [ -z "$sock" ]; then
    echo "sway failed to create a Wayland socket. Log:" >&2
    cat /tmp/sway.log >&2 || true
    exit 1
fi
export WAYLAND_DISPLAY="$(basename "$sock")"
unset DISPLAY
echo "WAYLAND_DISPLAY=$WAYLAND_DISPLAY"

# 1b. Start pipewire — xdg-desktop-portal-wlr will fail to expose its
# Screenshot and ScreenCast interfaces if it can't talk to a pipewire core
# (ScreenCast needs the pipewire graph; Screenshot init is gated on the
# same startup path).
pipewire >/tmp/pipewire.log 2>&1 &
CLEANUP_PIDS+=($!)
wireplumber >/tmp/wireplumber.log 2>&1 &
CLEANUP_PIDS+=($!)
# Wait briefly for pipewire to come up on the session bus.
for _ in $(seq 1 25); do
    if pw-cli info 0 >/dev/null 2>&1; then
        break
    fi
    sleep 0.2
done

# 2. Start the portal infrastructure. xdg-desktop-portal-wlr exports the
# Screenshot interface for wlroots compositors; xdg-desktop-portal is the
# frontend that sits on the session bus.
#
# The portal frontend picks a backend based on XDG_CURRENT_DESKTOP and a
# /usr/share/xdg-desktop-portal/portals/<name>.portal manifest. The wlr
# package ships wlr.portal with UseIn=sway (amongst others), so setting
# XDG_CURRENT_DESKTOP=sway above makes the frontend route screenshot
# requests to xdg-desktop-portal-wlr.
#
# Ubuntu 24.04's shipped sway-portals.conf leaves Screenshot unbound in
# practice (only ScreenCast gets routed to wlr). Write an explicit override
# into a user-scoped config so Screenshot reaches the wlr backend too.
mkdir -p /root/.config/xdg-desktop-portal
cat > /root/.config/xdg-desktop-portal/sway-portals.conf <<'EOF'
[preferred]
default=wlr;gtk
# Screenshot requires an Access portal (for consent UI) which wlr doesn't
# implement — fall back to gtk for Access while keeping wlr for Screenshot
# itself.
org.freedesktop.impl.portal.Screenshot=wlr
org.freedesktop.impl.portal.ScreenCast=wlr
org.freedesktop.impl.portal.Access=gtk
EOF
/usr/libexec/xdg-desktop-portal-wlr -l TRACE >/tmp/portal-wlr.log 2>&1 &
CLEANUP_PIDS+=($!)
/usr/libexec/xdg-desktop-portal-gtk >/tmp/portal-gtk.log 2>&1 &
CLEANUP_PIDS+=($!)
/usr/libexec/xdg-desktop-portal -v >/tmp/portal.log 2>&1 &
CLEANUP_PIDS+=($!)

# xdg-desktop-portal registers its portal objects lazily as the impl
# backends respond on the bus. In a container without a systemd user unit
# or graphical session, the chain (pipewire → xdpw → xdp frontend) takes a
# few seconds to stabilise. Sleep then introspect — a single check is more
# reliable here than polling, which in practice races the build artefacts
# being mmap'd.
sleep 10
echo "---Screenshot interface check---"
if dbus-send --session --print-reply \
     --dest=org.freedesktop.portal.Desktop \
     /org/freedesktop/portal/desktop \
     org.freedesktop.DBus.Introspectable.Introspect 2>&1 \
     | grep -q 'org\.freedesktop\.portal\.Screenshot'; then
    echo "OK: portal Screenshot interface is registered."
else
    echo "WARN: portal Screenshot interface not registered; test will fail."
fi

# Dump the logs on the way in so failures are self-explanatory.
echo "---portal-wlr log---"
tail -20 /tmp/portal-wlr.log 2>/dev/null || true
echo "---portal log---"
tail -20 /tmp/portal.log 2>/dev/null || true
echo "---end logs---"

echo "---portal interfaces at /org/freedesktop/portal/desktop---"
dbus-send --session --print-reply --dest=org.freedesktop.portal.Desktop \
    /org/freedesktop/portal/desktop \
    org.freedesktop.DBus.Introspectable.Introspect 2>&1 \
    | grep -E 'interface name|method name=' | head -40 || true
echo "---end interfaces---"
echo "---impl-side: does xdpw expose Screenshot impl?---"
dbus-send --session --print-reply --dest=org.freedesktop.impl.portal.desktop.wlr \
    /org/freedesktop/portal/desktop \
    org.freedesktop.DBus.Introspectable.Introspect 2>&1 \
    | grep -E 'interface name=' | head -20 || true
echo "---end impl-side---"

# 3. Build the workspace and launch the AccessKit test app so the
# element-capture test has a live a11y tree to aim at. We enable AT-SPI2
# on the bus first — same as run_integ_tests.sh does for X11.
export NO_AT_BRIDGE=0
export AT_SPI_CLIENT=true
export ACCESSIBILITY_ENABLED=1
if command -v /usr/libexec/at-spi-bus-launcher &>/dev/null; then
    /usr/libexec/at-spi-bus-launcher --launch-immediately &
    CLEANUP_PIDS+=($!)
fi
if command -v /usr/libexec/at-spi2-registryd &>/dev/null; then
    /usr/libexec/at-spi2-registryd &
    CLEANUP_PIDS+=($!)
fi
sleep 1
dbus-send --session --print-reply --dest=org.a11y.Bus /org/a11y/bus \
    org.freedesktop.DBus.Properties.Set \
    string:org.a11y.Status string:IsEnabled variant:boolean:true \
    2>/dev/null || true
dbus-send --session --print-reply --dest=org.a11y.Bus /org/a11y/bus \
    org.freedesktop.DBus.Properties.Set \
    string:org.a11y.Status string:ScreenReaderEnabled variant:boolean:true \
    2>/dev/null || true

cd /xa11y
echo "Building workspace..."
cargo build --workspace --features xa11y/strict-roles >/tmp/build.log 2>&1

echo "Launching xa11y-test-app..."
./target/debug/xa11y-test-app --headless >/tmp/test-app.log 2>&1 &
CLEANUP_PIDS+=($!)
sleep 3

set +e
timeout 60 cargo test -p xa11y --test integ_test --features strict-roles \
    -- --ignored --test-threads=1 --nocapture capture_
rc=$?
set -e

echo "---portal-wlr log (final)---"
tail -40 /tmp/portal-wlr.log 2>/dev/null || true
echo "---portal log (final)---"
tail -40 /tmp/portal.log 2>/dev/null || true
echo "---sway log (final)---"
tail -20 /tmp/sway.log 2>/dev/null || true
echo "---end---"
exit $rc

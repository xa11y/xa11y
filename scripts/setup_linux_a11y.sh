#!/usr/bin/env bash
# Set up a Linux accessibility environment in the current D-Bus session.
#
# Two usage modes:
#
#   # 1. Run a single command:
#   dbus-run-session -- bash scripts/setup_linux_a11y.sh -- cmd args...
#
#   # 2. Prime the current dbus-session and run multiple commands later
#   #    (used by CI when running several examples in the same session):
#   source scripts/setup_linux_a11y.sh
#   cargo run ...
#   python ...
#
# Caller is responsible for `Xvfb` (DISPLAY) and `dbus-run-session` already
# being in effect — this helper assumes DBUS_SESSION_BUS_ADDRESS is set. We
# start at-spi-bus-launcher + at-spi2-registryd, flip the AT-SPI Status flags
# to "enabled", and (in mode 1) exec the caller's command.
#
# Mirrors the setup inlined in scripts/run_integ_tests.sh.

if [ -z "${DBUS_SESSION_BUS_ADDRESS:-}" ]; then
    echo "setup_linux_a11y.sh: DBUS_SESSION_BUS_ADDRESS unset — run me under dbus-run-session" >&2
    return 1 2>/dev/null || exit 1
fi

export NO_AT_BRIDGE=0
export AT_SPI_CLIENT=true
export ACCESSIBILITY_ENABLED=1

_xa11y_find_atspi_bin() {
    local exe="$1"
    if [ -x "/usr/libexec/$exe" ]; then
        echo "/usr/libexec/$exe"
    elif command -v "$exe" >/dev/null 2>&1; then
        command -v "$exe"
    fi
}

_xa11y_bus_launcher=$(_xa11y_find_atspi_bin at-spi-bus-launcher)
if [ -n "${_xa11y_bus_launcher:-}" ]; then
    "$_xa11y_bus_launcher" --launch-immediately &
else
    echo "setup_linux_a11y.sh: at-spi-bus-launcher not found" >&2
fi
sleep 1

_xa11y_registryd=$(_xa11y_find_atspi_bin at-spi2-registryd)
if [ -n "${_xa11y_registryd:-}" ]; then
    "$_xa11y_registryd" &
else
    echo "setup_linux_a11y.sh: at-spi2-registryd not found" >&2
fi
sleep 1

# Flip the AT-SPI status flags to "enabled" so clients see the tree. Ignore
# failure — the daemons sometimes haven't registered the Status object yet
# at this moment, and the flags default-on in newer at-spi2-core anyway.
dbus-send --session --print-reply --dest=org.a11y.Bus /org/a11y/bus \
    org.freedesktop.DBus.Properties.Set \
    string:org.a11y.Status string:IsEnabled variant:boolean:true \
    2>/dev/null || true
dbus-send --session --print-reply --dest=org.a11y.Bus /org/a11y/bus \
    org.freedesktop.DBus.Properties.Set \
    string:org.a11y.Status string:ScreenReaderEnabled variant:boolean:true \
    2>/dev/null || true

# Mode 1: `... -- cmd args` — exec the command.
if [ $# -gt 0 ] && [ "$1" = "--" ]; then
    shift
    exec "$@"
fi

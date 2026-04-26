#!/usr/bin/env bash
# End-to-end validation for xa11y-linux's Wayland input-sim backend.
#
# The backend writes to /dev/uinput; CI runs this script inside the
# `xa11y-wayland-uinput` container with `--device /dev/uinput` so the
# kernel uinput node is reachable. The container also has `libevdev` so
# the test reader can scan /dev/input/event* for the virtual device the
# backend registers.
#
# Two test passes:
#   1. cargo test -p xa11y-linux                   (lib + smoke)
#   2. cargo test -p xa11y-linux --test wayland_input_e2e -- --ignored
#         (the real e2e tests — write events through LinuxInputProvider,
#          read them back via evdev, assert codes/values match)

exec 1> >(stdbuf -o0 cat) 2>&1
set -euo pipefail

cd /xa11y

echo "--- /dev/uinput probe ---"
ls -l /dev/uinput || {
    echo "FAIL: /dev/uinput is not present in the container."
    echo "      Re-run docker with --device /dev/uinput."
    exit 1
}

echo "--- evdev / xkbcommon library probe ---"
ldconfig -p | grep -E 'libevdev|libxkbcommon' | head -10 || true

echo "--- cargo build -p xa11y-linux ---"
cargo build -p xa11y-linux --tests

echo "--- cargo test -p xa11y-linux --lib + smoke ---"
cargo test -p xa11y-linux --lib
cargo test -p xa11y-linux --test wayland_smoke

echo "--- cargo test -p xa11y-linux --test wayland_input_e2e -- --ignored ---"
# The `#[ignore]`'d e2e tests open /dev/uinput, register a virtual
# evdev device, drive the public InputProvider methods, then read
# events back via evdev and assert the wire-level output. They run
# single-threaded because they share `/dev/uinput` and the resulting
# `/dev/input/eventN` node is reused across tests.
unset DISPLAY
cargo test -p xa11y-linux --test wayland_input_e2e -- \
    --ignored --test-threads=1 --nocapture

echo "OK: uinput container build + unit + e2e tests passed."

#!/usr/bin/env bash
# Validate that xa11y-linux compiles and its unit tests pass inside a
# container that has the libei + xkbcommon system libraries installed.
#
# Originally this script tried to drive the full Wayland portal stack
# (mutter --headless + xdg-desktop-portal-gnome + gnome-remote-desktop)
# end-to-end so we could run the `wayland_input_e2e` tests in CI.
# Headless GNOME portal RemoteDesktop reliably needs a GDM-managed
# session; getting it to work in a stock Ubuntu CI container without
# systemd or GDM proved to be considerably more work than the value we
# get from running those tests in CI vs. on a developer's real GNOME
# Wayland session. The e2e tests stay `#[ignore]`'d in
# `xa11y-linux/tests/wayland_input_e2e.rs` and run on demand for
# developers with a working portal stack.
#
# What this script DOES validate (which the regular `linux` CI job
# doesn't):
#   * the libei container Containerfile builds
#   * `xa11y-linux` links against the system `libei`/`libxkbcommon` /
#     `libeis` packages installed by `Containerfile.wayland-libei`
#   * the in-process Wayland-input tests in `src/wayland_input.rs::tests`
#     pass against those same system libraries
#
# This catches regressions in the libei container packaging and confirms
# that a future libei system-library SONAME bump won't silently break
# xa11y at link time.

exec 1> >(stdbuf -o0 cat) 2>&1
set -euo pipefail

cd /xa11y

echo "--- libei container library probe ---"
ldconfig -p | grep -E 'libei|libxkbcommon' | head -10 || true

echo "--- cargo build -p xa11y-linux ---"
cargo build -p xa11y-linux

echo "--- cargo test -p xa11y-linux --lib ---"
# Library tests cover the in-process Wayland-input unit tests
# (`wayland_input::tests::*`), which exercise xkbcommon keymap
# enumeration and the reverse-keysym map without needing a portal.
cargo test -p xa11y-linux --lib

echo "--- cargo test -p xa11y-linux --test wayland_smoke ---"
# Env-routing smoke (no compositor needed). Confirms the new Wayland
# input branch is reached when WAYLAND_DISPLAY is set without DISPLAY.
cargo test -p xa11y-linux --test wayland_smoke

echo "OK: libei container build + unit tests passed."

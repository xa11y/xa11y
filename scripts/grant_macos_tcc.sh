#!/usr/bin/env bash
# Grant the macOS Accessibility (TCC) permission to a given client binary.
#
# macOS gates the Accessibility API (AXUIElement, the thing xa11y-macos drives,
# and CGEventPost, the thing input simulation drives) behind the TCC privacy
# database. A process that hasn't been granted kTCCServiceAccessibility reads
# back empty trees and silently drops synthesised events. On a developer box
# you grant this once via System Settings → Privacy & Security → Accessibility.
# In headless / CI contexts there's no UI to click, so we write the grant
# straight into the system TCC database.
#
# Usage:
#   scripts/grant_macos_tcc.sh <client-path>
#
#   <client-path>  Absolute path to the executable that calls the AX API.
#                  For a venv-based test run this is the *resolved* interpreter
#                  (follow symlinks), e.g.
#                      scripts/grant_macos_tcc.sh \
#                        "$(.venv/bin/python -c 'import sys; print(sys.executable)')"
#                  TCC matches on the real on-disk binary, so pass the resolved
#                  path, not a symlink in a venv's bin/.
#
# Requirements:
#   - macOS, and sudo that can write the system TCC db. GitHub-hosted macOS
#     runners allow this (SIP only protects /System/*). On a locked-down
#     corporate Mac the sqlite write will fail — use System Settings instead.
#
# ── Why this lives in scripts/ and is ALSO duplicated inside the
#    .github/actions/setup-a11y composite action ──────────────────────────
#
# The setup-a11y action carries its own inline copy of this exact sqlite
# INSERT (see its "Configure macOS accessibility permission" step). That looks
# like a Tenet-1 duplication, but it's structural, not accidental:
#
#   * A composite action consumed by an EXTERNAL repo as
#       uses: xa11y/xa11y/.github/actions/setup-a11y@main
#     only receives the action's own directory via $GITHUB_ACTION_PATH. It
#     does NOT get a checkout of the rest of this repo, so it cannot
#     `bash scripts/grant_macos_tcc.sh` — that file simply isn't there for an
#     external consumer.
#   * Making the action self-contained (inline copy) is therefore required for
#     it to work for the people it's published for.
#   * This script exists for the contexts the action can't serve: local macOS
#     development and the standalone shell harnesses (run_integ_tests_macos.sh
#     et al.) that run outside Actions.
#
# So there are intentionally two copies — the action's inline one and this
# one — and they must be kept in sync. When you change the INSERT here, mirror
# it in .github/actions/setup-a11y/action.yml.

set -euo pipefail

if [ "$(uname)" != "Darwin" ]; then
    echo "grant_macos_tcc.sh: not macOS ($(uname)) — nothing to do." >&2
    exit 0
fi

CLIENT="${1:-}"
if [ -z "$CLIENT" ]; then
    echo "usage: $0 <client-path>" >&2
    exit 2
fi

echo "Granting kTCCServiceAccessibility to: $CLIENT"

# Named columns keep the INSERT working across macOS versions regardless of
# how many extra columns (boots_count, is_deleted on macOS 14+, …) the access
# table grows. auth_value=2 is "allowed"; client_type=1 is "absolute path".
sudo sqlite3 "/Library/Application Support/com.apple.TCC/TCC.db" \
    "INSERT OR REPLACE INTO access(service,client,client_type,auth_value,auth_reason,auth_version,csreq,policy_id,indirect_object_identifier_type,indirect_object_identifier,indirect_object_code_identity,flags,last_modified) \
     VALUES('kTCCServiceAccessibility','${CLIENT}',1,2,4,1,NULL,NULL,0,'UNUSED',NULL,0,$(date +%s));"

# Restart tccd so the grant is picked up without waiting for the next natural
# reload cycle. launchctl stop triggers an auto-restart.
sudo launchctl stop com.apple.tccd 2>/dev/null || true
sleep 2

echo "grant_macos_tcc.sh: done."

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
#    xa11y/setup-a11y composite action ───────────────────────────────────────
#
# The setup-a11y action (published from the xa11y/setup-a11y repo) carries its
# own inline copy of this exact sqlite INSERT (see its "Configure macOS
# accessibility permission" step). That looks like a Tenet-1 duplication, but
# it's structural, not accidental:
#
#   * A composite action consumed as
#       uses: xa11y/setup-a11y@v1
#     only receives the action's own directory via $GITHUB_ACTION_PATH. It
#     does NOT get a checkout of this repo, so it cannot
#     `bash scripts/grant_macos_tcc.sh` — that file simply isn't there for an
#     external consumer.
#   * Making the action self-contained (inline copy) is therefore required for
#     it to work for the people it's published for.
#   * This script exists for the contexts the action can't serve: local macOS
#     development and the standalone shell harnesses (run_integ_tests_macos.sh
#     et al.) that run outside Actions.
#   * This repo's own CI (the macOS cells of the `integ` matrix in ci.yml)
#     also calls this script rather than the action, because the action's
#     inline copy has no busy timeout or retry and intermittently failed on
#     "database is locked (5)" when tccd held TCC.db.
#
# So there are intentionally two copies — the action's inline one and this
# one — and they must be kept in sync. When you change the INSERT here, mirror
# it in the xa11y/setup-a11y repo's action.yml.

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

TCC_DB="/Library/Application Support/com.apple.TCC/TCC.db"

# tccd keeps TCC.db open and writes to it on its own schedule, so our write can
# collide with one of its transactions and fail with "database is locked (5)"
# (SQLITE_BUSY). That is transient contention, not a refusal, and it was
# intermittently failing the macOS integ cells in CI. Two layers handle it:
#
#   * `.timeout` installs sqlite's busy handler, so one attempt waits up to
#     TCC_BUSY_TIMEOUT_MS for tccd's lock to clear instead of failing on first
#     contact.
#   * A bounded retry covers a lock held past that window (tccd mid-restart, a
#     long write). Every failed attempt is printed, and the last one exits
#     non-zero: a grant that did not land must fail here, where the cause is
#     visible, rather than later as an empty accessibility tree.
TCC_BUSY_TIMEOUT_MS="${TCC_BUSY_TIMEOUT_MS:-10000}"
TCC_WRITE_ATTEMPTS="${TCC_WRITE_ATTEMPTS:-5}"

# A single quote in the path would end the SQL string literal early.
CLIENT_SQL="$(printf '%s' "$CLIENT" | sed "s/'/''/g")"

# Named columns keep the INSERT working across macOS versions regardless of
# how many extra columns (boots_count, is_deleted on macOS 14+, …) the access
# table grows. auth_value=2 is "allowed"; client_type=1 is "absolute path".
GRANT_SQL="INSERT OR REPLACE INTO access(service,client,client_type,auth_value,auth_reason,auth_version,csreq,policy_id,indirect_object_identifier_type,indirect_object_identifier,indirect_object_code_identity,flags,last_modified) \
 VALUES('kTCCServiceAccessibility','${CLIENT_SQL}',1,2,4,1,NULL,NULL,0,'UNUSED',NULL,0,$(date +%s));"
VERIFY_SQL="SELECT auth_value FROM access WHERE service='kTCCServiceAccessibility' AND client='${CLIENT_SQL}' AND client_type=1;"

tcc_sqlite() {
    sudo sqlite3 -cmd ".timeout ${TCC_BUSY_TIMEOUT_MS}" "$TCC_DB" "$1"
}

attempt=1
until err="$(tcc_sqlite "$GRANT_SQL" 2>&1)"; do
    echo "grant_macos_tcc.sh: TCC.db write attempt ${attempt}/${TCC_WRITE_ATTEMPTS} failed: ${err}" >&2
    if [ "$attempt" -ge "$TCC_WRITE_ATTEMPTS" ]; then
        echo "::error::grant_macos_tcc.sh: could not write the kTCCServiceAccessibility grant for ${CLIENT} after ${TCC_WRITE_ATTEMPTS} attempts (busy timeout ${TCC_BUSY_TIMEOUT_MS}ms each). Last error: ${err}" >&2
        exit 1
    fi
    sleep $((attempt * 2))
    attempt=$((attempt + 1))
done

# Read the row back. A write that reported success but left no allowed row
# would otherwise surface only as empty trees once the tests run.
granted="$(tcc_sqlite "$VERIFY_SQL")"
if [ "$granted" != "2" ]; then
    echo "::error::grant_macos_tcc.sh: TCC.db holds no allowed kTCCServiceAccessibility row for ${CLIENT} after the write (auth_value read back: '${granted}')" >&2
    exit 1
fi

# Restart tccd so the grant is picked up without waiting for the next natural
# reload cycle. launchctl stop triggers an auto-restart.
sudo launchctl stop com.apple.tccd 2>/dev/null || true
sleep 2

echo "grant_macos_tcc.sh: done."

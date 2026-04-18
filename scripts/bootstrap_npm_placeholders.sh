#!/usr/bin/env bash
#
# One-time bootstrap: publish minimal placeholder versions of each
# @crowecawcaw/xa11y* package so that npm trusted publishing can be
# configured per-package on npmjs.com.
#
# npm requires a package to exist before you can attach a trusted
# publisher to it. Each placeholder is published under the `placeholder`
# dist-tag so it does not become `latest`; the real CI release at the
# current workspace version will be the first thing users install.
#
# Usage:
#   npm login                            # if not already logged in
#   ./scripts/bootstrap_npm_placeholders.sh
#
# After running this and configuring trusted publishing for each of the
# 7 package names on https://www.npmjs.com/, you can delete this script.

set -euo pipefail

SCOPE="@crowecawcaw"
PLACEHOLDER_VERSION="0.0.1-placeholder.0"

PACKAGES=(
  "xa11y"
  "xa11y-linux-x64-gnu"
  "xa11y-linux-arm64-gnu"
  "xa11y-darwin-x64"
  "xa11y-darwin-arm64"
  "xa11y-win32-x64-msvc"
  "xa11y-win32-arm64-msvc"
)

if ! npm whoami >/dev/null 2>&1; then
  echo "ERROR: not logged in to npm. Run 'npm login' first." >&2
  exit 1
fi

# 2FA-protected accounts need an OTP. Pass via NPM_OTP env var; a single
# OTP is reused for all 7 publishes (npm caches it for the session).
OTP_ARG=()
if [[ -n "${NPM_OTP:-}" ]]; then
  OTP_ARG=(--otp "$NPM_OTP")
fi

ME=$(npm whoami)
echo "Logged in as: $ME"
echo "Will publish placeholders for ${#PACKAGES[@]} packages under $SCOPE/"
echo

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

for pkg in "${PACKAGES[@]}"; do
  full="$SCOPE/$pkg"

  # Skip if the package already exists on the registry.
  if npm view "$full" version >/dev/null 2>&1; then
    echo "SKIP $full (already exists on npm)"
    continue
  fi

  dir="$TMP/$pkg"
  mkdir -p "$dir"
  cat > "$dir/package.json" <<EOF
{
  "name": "$full",
  "version": "$PLACEHOLDER_VERSION",
  "description": "Placeholder reserving the package name. The real release will arrive shortly. See https://github.com/xa11y/xa11y",
  "license": "MIT",
  "repository": {
    "type": "git",
    "url": "https://github.com/xa11y/xa11y"
  },
  "homepage": "https://xa11y.dev"
}
EOF

  echo "PUBLISH $full @ $PLACEHOLDER_VERSION (tag: placeholder)"
  (cd "$dir" && npm publish --access public --tag placeholder ${OTP_ARG[@]+"${OTP_ARG[@]}"})
done

echo
echo "Done. Next steps:"
echo "  1. For each of the 7 packages above, go to https://www.npmjs.com/package/<name>/access"
echo "     and add a Trusted Publisher (GitHub Actions, repo xa11y/xa11y, workflow publish.yml)."
echo "  2. Run the Publish workflow on GitHub. It will use OIDC (no NPM_TOKEN needed)."

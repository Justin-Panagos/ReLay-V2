#!/usr/bin/env bash
# Builds a clean extension zip ready for Chrome Web Store and Firefox AMO submission.
# Run from the repo root: ./scripts/build-extension.sh
#
# Output: relay-extension-<version>.zip in the repo root.
# Excludes: extension_key.pem, _metadata/, .DS_Store, and any editor backup files.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
EXT_DIR="$REPO_ROOT/extension"

# Read version from manifest.json
VERSION=$(node -p "require('$EXT_DIR/manifest.json').version" 2>/dev/null || echo "0.1.0")
OUTPUT="$REPO_ROOT/relay-extension-$VERSION.zip"

echo "Building extension v$VERSION..."

# Remove any previous build
rm -f "$OUTPUT"

# Build zip from the extension directory, excluding files that must not be shipped
cd "$EXT_DIR"
zip -r "$OUTPUT" . \
  --exclude "extension_key.pem" \
  --exclude "_metadata/*" \
  --exclude ".DS_Store" \
  --exclude "*.swp" \
  --exclude "*.swo" \
  --exclude "*~"

echo "Done: $OUTPUT"
echo ""
echo "Chrome Web Store checklist:"
echo "  1. Upload $OUTPUT at https://chrome.google.com/webstore/devconsole"
echo "  2. Privacy policy URL: https://justin-panangos.github.io/ReLay/privacy"
echo "  3. Screenshots: 1280x800px (take 3-4 showing popup, download in progress, install prompt)"
echo "  4. Category: Productivity"
echo ""
echo "Firefox AMO:"
echo "  1. Upload the same zip at https://addons.mozilla.org/developers"
echo "  2. Firefox auto-converts MV3 — no separate build needed"

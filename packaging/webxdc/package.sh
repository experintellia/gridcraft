#!/usr/bin/env bash
# Build GridCraft as a webxdc app (Delta Chat and other messengers):  $DIST/gridcraft-<version>.xdc
#
# Usage: packaging/webxdc/package.sh [--skip-build]
#
# Needs what packaging/web/package.sh needs. Everyone in a chat who starts the app edits one
# shared workbook (apps/gridcraft-web/src/collab.rs).
set -euo pipefail
# shellcheck source=../env.sh
. "$(dirname "${BASH_SOURCE[0]}")/../env.sh"

SITE="$CARGO_TARGET_DIR/webxdc-site"
if [ "${1:-}" != "--skip-build" ]; then
  command -v trunk >/dev/null || { echo "error: trunk not found (cargo install trunk --locked)" >&2; exit 1; }
  # Trunk writes the page as index.html, where the messenger looks for it.
  (cd "$ROOT/apps/gridcraft-web" && trunk build webxdc.html --release --dist "$SITE" --public-url ./)
fi
[ -f "$SITE/index.html" ] || { echo "error: $SITE/index.html missing; run without --skip-build" >&2; exit 1; }

cp -f "$ROOT/assets/app-icon/gridcraft-macos-512.png" "$SITE/icon.png"
cat > "$SITE/manifest.toml" <<TOML
name = "GridCraft"
source_code_url = "https://github.com/experintellia/gridcraft"
TOML
copy_docs "$SITE"
OUT="$DIST/gridcraft-$VERSION.xdc"
rm -f "$OUT"
(cd "$SITE" && zip -qr9 "$OUT" .)
echo "wrote $OUT"
ls -lh "$OUT"

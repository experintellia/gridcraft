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
# Ship the wasm gzipped and stored (zip -n .gz below): Delta Chat inflates zip entries slowly.
# Served through deltachat-rpc-server, the 11.8 MB wasm arrived after ~2.7 s, its 4.7 MB .gz
# after ~0.4 s; the browser gunzips it natively while compiling.
# ponytail: needs DecompressionStream (Chrome 80, Safari 16.4, Firefox 113); no fallback.
for wasm in "$SITE"/*_bg.wasm; do
  [ -f "$wasm" ] || continue
  name=$(basename "$wasm")
  gzip -9 -n -f "$wasm"
  NAME="$name" perl -pi -e '
    s{<link rel="preload" href="\./\Q$ENV{NAME}\E"[^>]*>}{};
    s{\x27\./\Q$ENV{NAME}\E\x27}{fetch("./$ENV{NAME}.gz").then(r => new Response(r.body.pipeThrough(new DecompressionStream("gzip")), { headers: { "content-type": "application/wasm" } }))};
  ' "$SITE/index.html"
done

cp -f "$ROOT/assets/app-icon/gridcraft-macos-512.png" "$SITE/icon.png"
cat > "$SITE/manifest.toml" <<TOML
name = "GridCraft"
source_code_url = "https://github.com/experintellia/gridcraft"
TOML
copy_docs "$SITE"
OUT="$DIST/gridcraft-$VERSION.xdc"
rm -f "$OUT"
# No -9: in zip 3.0 it overrides -n, which would deflate the .gz again.
(cd "$SITE" && zip -qr -n .gz "$OUT" .)
echo "wrote $OUT"
ls -lh "$OUT"

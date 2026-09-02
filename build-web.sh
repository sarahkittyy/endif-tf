#!/usr/bin/env bash
pause_on_exit() {
  local status=$?
  if [ -t 0 ] && [ -z "${ENDIF_NOPAUSE:-}" ]; then
    echo
    if [ "$status" -eq 0 ]; then echo "build-web.sh finished OK."; else echo "build-web.sh FAILED (exit status $status)."; fi
    echo "Press any key to close this window..."
    read -rsn1 || true
  fi
  exit "$status"
}
trap pause_on_exit EXIT
set -euo pipefail
cd "$(dirname "$0")"
if [ -f .env ]; then
  while IFS= read -r line || [ -n "$line" ]; do
    case "$line" in ''|'#'*) continue ;; esac
    key="${line%%=*}"; value="${line#*=}"
    key="$(printf '%s' "$key" | tr -d '[:space:]')"
    value="$(printf '%s' "$value" | tr -d '\r' | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//')"
    case "$key" in ENDIF_*) [ -z "${!key:-}" ] && export "$key=$value" ;; esac
  done < .env
fi
PROFILE="${1:-wasm-release}"
BACKENDS="${2:-webgpu webgl2}"
case "$PROFILE" in wasm-dev) OPTIMIZE="${OPTIMIZE:-0}" ;; *) OPTIMIZE="${OPTIMIZE:-1}" ;; esac
PY="$(command -v python3 || command -v python)"

ASSET_HASH="$("$PY" - <<'EOF'
import hashlib, os
h = hashlib.sha1()
root = "crates/client/assets"
for d, _, files in sorted(os.walk(root)):
    for f in sorted(files):
        p = os.path.join(d, f)
        h.update(os.path.relpath(p, root).replace(os.sep, "/").encode())
        h.update(b"\0")
        with open(p, "rb") as fh:
            h.update(fh.read())
print(h.hexdigest()[:10])
EOF
)"
export ENDIF_ASSET_DIR="assets-$ASSET_HASH"

WASM_OPT="${WASM_OPT:-}"
HAVE_BROTLI=0
if [ "$OPTIMIZE" = 1 ]; then
  if [ -z "$WASM_OPT" ]; then
    for cand in wasm-opt "$HOME/.cargo/bin/wasm-opt" "${LOCALAPPDATA:-}/.wasm-pack"/wasm-opt-*/bin/wasm-opt "$HOME/.cache/.wasm-pack"/wasm-opt-*/bin/wasm-opt; do
      if command -v "$cand" >/dev/null 2>&1; then WASM_OPT="$cand"; break; fi
    done
  fi
  if [ -z "$WASM_OPT" ]; then
    echo "wasm-opt not found; skipping (install with: cargo install wasm-opt, or set WASM_OPT=/path/to/wasm-opt)"
  fi
  if command -v brotli >/dev/null 2>&1; then HAVE_BROTLI=1; else
    echo "brotli not found; only .gz will be written (apt install brotli / brew install brotli / cargo install brotli)"
  fi
else
  WASM_OPT=""
  echo "OPTIMIZE=0: skipping wasm-opt and gzip/brotli (dev build, do not deploy)"
fi

STAGE="target/web-stage"
rm -rf "$STAGE"
mkdir -p "$STAGE" dist

compress() {
  gzip -9 -k -f "$1"
  if [ "$HAVE_BROTLI" = 1 ]; then brotli -f -q 9 -o "$1.br" "$1"; fi
}

build() {
  local backend="$1"
  echo "== building endif-$backend ($PROFILE, $ENDIF_ASSET_DIR)"
  cargo build -p endif-client --target wasm32-unknown-unknown --profile "$PROFILE" --no-default-features --features "$backend"
  wasm-bindgen --target web --out-dir "$STAGE" --out-name "endif-$backend" --no-typescript \
    "target/wasm32-unknown-unknown/$PROFILE/endif-client.wasm"
  "$PY" tools/strip_comments.py "$STAGE/endif-$backend.js"
  if [ -n "$WASM_OPT" ]; then
    echo "running wasm-opt ($WASM_OPT)"
    "$WASM_OPT" -Oz --enable-bulk-memory --enable-nontrapping-float-to-int --enable-sign-ext --enable-mutable-globals \
      --enable-reference-types --enable-multivalue -o "$STAGE/endif-${backend}_bg.wasm" "$STAGE/endif-${backend}_bg.wasm"
  fi
  if [ "$OPTIMIZE" = 1 ]; then
    compress "$STAGE/endif-${backend}_bg.wasm"
    compress "$STAGE/endif-$backend.js"
  fi
}

for backend in $BACKENDS; do build "$backend"; done

wasm_bytes() {
  local f="$STAGE/endif-$1_bg.wasm"
  [ -f "$f" ] || f="dist/endif-$1_bg.wasm"
  if [ -f "$f" ]; then wc -c < "$f" | tr -d " "; else echo 0; fi
}

cp web/index.html "$STAGE/index.html"
"$PY" tools/strip_comments.py "$STAGE/index.html"
WEBGPU_BYTES=$(wasm_bytes webgpu)
WEBGL2_BYTES=$(wasm_bytes webgl2)
sed -i.bak "s/__WASM_BYTES_WEBGPU__/$WEBGPU_BYTES/; s/__WASM_BYTES_WEBGL2__/$WEBGL2_BYTES/" "$STAGE/index.html"
sed -i.bak "s|__ASSET_DIR__|$ENDIF_ASSET_DIR|g" "$STAGE/index.html"
sed -i.bak "s|__SIGNALING__|${ENDIF_SIGNALING:-}|" "$STAGE/index.html"
# Cache buster on the script and wasm URLs: unique per build, committed or not.
CACHE_BUST="$(git rev-parse --short HEAD 2>/dev/null || true)-$(date +%s)"
sed -i.bak "s|__CACHE_BUST__|$CACHE_BUST|g" "$STAGE/index.html"
# The page compares itself to the server's /build before downloading the wasm (index.html); the
# same value the wasm carries as endif_sim::BUILD_ID.
BUILD_ID="$(cargo run -q --release -p endif-sim --bin build-id)"
sed -i.bak "s|__BUILD_ID__|$BUILD_ID|g" "$STAGE/index.html"
rm -f "$STAGE/index.html.bak"

if [ ! -d "dist/$ENDIF_ASSET_DIR" ]; then
  cp -r crates/client/assets "dist/$ENDIF_ASSET_DIR.tmp" && mv "dist/$ENDIF_ASSET_DIR.tmp" "dist/$ENDIF_ASSET_DIR"
fi
for d in dist/assets dist/assets-*; do
  [ -e "$d" ] && [ "$d" != "dist/$ENDIF_ASSET_DIR" ] && rm -rf "$d"
done
find dist -maxdepth 1 -name 'endif-*' ! -name 'endif-webgpu*' ! -name 'endif-webgl2*' -exec rm -f {} +
for backend in $BACKENDS; do rm -f dist/endif-"$backend".js* dist/endif-"$backend"_bg.wasm*; done
mv -f "$STAGE"/endif-* dist/
mv -f "$STAGE/index.html" dist/index.html
cp tools/ice-test.html dist/ice-test.html
rm -rf "$STAGE"

echo "web build ready in ./dist  (webgpu wasm $WEBGPU_BYTES bytes, webgl2 wasm $WEBGL2_BYTES bytes, $ENDIF_ASSET_DIR)"
echo "serve it with: $PY tools/serve.py   (or nginx, see web/nginx.example.conf)"

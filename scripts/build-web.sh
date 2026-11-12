#!/usr/bin/env bash
# Builds the playground into web/, ready to serve: python3 -m http.server -d web
#
# web/mattone.wasm (the emulator) comes from MATTONE_WASM if set, otherwise from a mattone
# checkout in ../mattone (built there with its own scripts/build-web.sh).
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build --release --lib --target wasm32-unknown-unknown
cp target/wasm32-unknown-unknown/release/cartuccia.wasm web/cartuccia.wasm
echo "web/cartuccia.wasm: $(wc -c < web/cartuccia.wasm) bytes"
mkdir -p web/examples
cp examples/*.cart web/examples/
# llms.txt: what an AI needs to write a game, also used by the playground's "Ask an AI"
{
  cat docs/llms-intro.md
  sed 's/^#/##/' docs/language.md
  for game in snake starfield; do
    printf '\n## Example: %s\n\n```lua\n' "$game"
    cat "examples/$game.cart"
    printf '```\n'
  done
} > web/llms.txt
echo "web/llms.txt: $(wc -c < web/llms.txt) bytes"
if [ -n "${MATTONE_WASM:-}" ]; then
  cp "$MATTONE_WASM" web/mattone.wasm
elif [ ! -f web/mattone.wasm ]; then
  cp ../mattone/web/mattone.wasm web/mattone.wasm
fi
echo "web/mattone.wasm: $(wc -c < web/mattone.wasm) bytes"

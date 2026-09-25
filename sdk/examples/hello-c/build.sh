#!/usr/bin/env bash
# Builds kumiko.hello-c into <out-dir>/plugin.wasm with wasi-sdk (WASI_SDK_PATH, by default
# ~/.local/wasi-sdk). The bindings in bindings/ come from ./regen-bindings.sh.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
sdk="${WASI_SDK_PATH:-$HOME/.local/wasi-sdk}"
out="${1:-$here/target}"
if [ ! -x "$sdk/bin/clang" ]; then
    echo "wasi-sdk not found at $sdk; set WASI_SDK_PATH" >&2
    exit 2
fi
mkdir -p "$out"
"$sdk/bin/clang" --target=wasm32-wasip2 -O2 -mexec-model=reactor -Wall -Wextra -Werror \
    -I "$here/bindings" -o "$out/plugin.wasm" \
    "$here/src/hello.c" "$here/bindings/kumiko_plugin.c" \
    "$here/bindings/kumiko_plugin_component_type.o"

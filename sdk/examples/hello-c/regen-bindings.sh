#!/usr/bin/env bash
# Regenerates bindings/ from sdk/wit after the WIT package changes.
# Needs wit-bindgen-cli of the version the Rust SDK uses: cargo install wit-bindgen-cli@0.62.0
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
wit-bindgen c "$here/../../wit/kumiko-plugin" --world kumiko-plugin --out-dir "$here/bindings"

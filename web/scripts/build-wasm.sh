#!/usr/bin/env bash
# Regenerate web/src/wasm from the estrogen-wasm crate. Run from anywhere; paths
# are resolved relative to this script. The files under web/src/wasm are the
# output of this script and are not meant to be hand-edited.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
web="$(dirname "$here")"
repo="$(dirname "$web")"
crate="$repo/crates/estrogen-wasm"
pkg="$crate/pkg"

if command -v wasm-pack >/dev/null 2>&1; then
	wasm-pack build "$crate" --target web --out-dir pkg --out-name estrogen
else
	# A standard Rust toolchain with wasm-bindgen CLI can build the same web
	# artifacts. Keep wasm-bindgen CLI at the version in Cargo.lock.
	cargo build --manifest-path "$repo/Cargo.toml" --release \
		--target wasm32-unknown-unknown -p estrogen-wasm
	mkdir -p "$pkg"
	wasm-bindgen "$repo/target/wasm32-unknown-unknown/release/estrogen_wasm.wasm" \
		--target web --out-dir "$pkg" --out-name estrogen
fi

dst="$web/src/wasm"
mkdir -p "$dst"
cp "$pkg/estrogen.js" "$dst/estrogen.js"
cp "$pkg/estrogen_bg.wasm" "$dst/estrogen_bg.wasm"
cp "$pkg/estrogen.d.ts" "$dst/estrogen.d.ts"
cp "$pkg/estrogen_bg.wasm.d.ts" "$dst/estrogen_bg.wasm.d.ts"

echo "copied wasm artifacts into $dst"

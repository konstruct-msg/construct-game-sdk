#!/usr/bin/env bash
# Build every game in games/ for wasm32, copy the modules to dist/, check them, and
# compare their hashes with GAMES.sha256.
#
#   scripts/build-games.sh            build, inspect, fail if a hash differs from GAMES.sha256
#   scripts/build-games.sh --update   build, inspect, rewrite GAMES.sha256
#
# A game's id is its hash, so GAMES.sha256 is tracked: a change that moves a hash shows
# up in the diff and is reviewed as what it is — a different game.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}"
TARGET=wasm32-unknown-unknown
UPDATE=0
[ "${1:-}" = "--update" ] && UPDATE=1

cd "$ROOT"

# Panic locations (file:line:col) are compiled into the module — stable Rust has no flag
# to drop them — so the checkout, the registry and the toolchain's own sources all end up
# in it as paths. Remapping each to a fixed prefix is what lets two machines, or two
# checkouts, produce the same bytes. The same strings are why a change that only moves
# lines (cargo fmt included) changes a game's hash.
#
# std's own paths are /rustc/<commit>/library/... — unless the rust-src component is
# installed, in which case rustc points them at the local copy instead. The last remap
# folds that copy back to the canonical form, so the bytes do not depend on whether a
# machine has rust-src. (When several remaps match, rustc applies the last.)
SYSROOT="$(rustc --print sysroot)"
RUSTC_COMMIT="$(rustc -vV | sed -n 's/^commit-hash: //p')"
export RUSTFLAGS="--remap-path-prefix=$ROOT=/construct-game-sdk --remap-path-prefix=$CARGO_HOME=/cargo --remap-path-prefix=$SYSROOT=/rust --remap-path-prefix=$SYSROOT/lib/rustlib/src/rust=/rustc/$RUSTC_COMMIT"

games=()
for manifest in games/*/Cargo.toml; do
    games+=("$(basename "$(dirname "$manifest")")")
done

packages=()
for game in "${games[@]}"; do packages+=(-p "$game"); done
cargo build --locked --release --target "$TARGET" "${packages[@]}"

mkdir -p dist
modules=()
for game in "${games[@]}"; do
    cp "target/$TARGET/release/$game.wasm" "dist/$game.wasm"
    modules+=("dist/$game.wasm")
done

python3 -I scripts/wasm_inspect.py "${modules[@]}"

# Two copies on one machine share $HOME, so check-reproducible.sh cannot see a home path
# that escaped the remapping above — it would only show as a different hash elsewhere.
if grep -l -a -F "$HOME" "${modules[@]}"; then
    echo "a module above contains a path under \$HOME — add a --remap-path-prefix for it" >&2
    exit 1
fi

(cd dist && shasum -a 256 "${games[@]/%/.wasm}") > dist/SHA256SUMS
if [ "$UPDATE" = 1 ]; then
    cp dist/SHA256SUMS GAMES.sha256
    echo "GAMES.sha256 updated"
elif ! diff -u GAMES.sha256 dist/SHA256SUMS; then
    echo "A game's hash changed. If that is intended, run with --update and commit GAMES.sha256." >&2
    exit 1
fi

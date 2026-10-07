#!/usr/bin/env bash
# Build every game in games/ for wasm32, copy the modules to dist/, check them, and
# compare their hashes with GAMES.sha256.
#
#   scripts/build-games.sh            build, inspect, fail if a hash differs from GAMES.sha256
#   scripts/build-games.sh --update   build, inspect, rewrite GAMES.sha256
#   scripts/build-games.sh --native   build on this machine whatever it is, for development:
#                                     dist/ is usable by the tests, GAMES.sha256 is left alone
#
# A game's id is its hash, so GAMES.sha256 is tracked: a change that moves a hash shows
# up in the diff and is reviewed as what it is — a different game.
#
# The hashes are those of the reference build: x86_64 Linux, rustc 1.96.0. Cargo mixes
# `rustc -vV` — which names the host — into every crate's metadata, and the metadata
# reaches the module's bytes (a version bump alone, same code, moved the hash), so a Mac
# and Linux build the same source to different games. On x86_64 Linux (CI) the script
# builds directly; anywhere else it re-runs itself in the pinned image below.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TARGET=wasm32-unknown-unknown
REFERENCE_HOST=x86_64-unknown-linux-gnu
# rust:1.96.0, the linux/amd64 manifest. By digest: a tag can be repointed.
REFERENCE_IMAGE=rust@sha256:477cee146d00e0dd888ce66a279ff78ccfd6fdf265e83414a4264652643c881a
MODE="${1:-check}"

cd "$ROOT"

# Off the reference host, the default is the emulated container. It is slow, and on the
# owner's Mac rustc hung in it twice at the same crate; if it does, use --native for
# development and take the reference hashes from CI's "Build games" step.
if [ "$MODE" != "--native" ] && [ "$(rustc -vV 2>/dev/null | sed -n 's/^host: //p')" != "$REFERENCE_HOST" ]; then
    command -v docker >/dev/null || {
        echo "not $REFERENCE_HOST and no docker: cannot make the reference build" >&2
        exit 1
    }
    # Mounted at its own absolute path, so check-reproducible.sh still builds from two
    # different paths. Named volumes keep the toolchain and the registry between runs.
    exec docker run --rm --platform linux/amd64 \
        -v "$ROOT:$ROOT" -w "$ROOT" \
        -v construct-game-sdk-rustup:/usr/local/rustup \
        -v construct-game-sdk-registry:/usr/local/cargo/registry \
        -e CARGO_TARGET_DIR="$ROOT/target/reference" \
        "$REFERENCE_IMAGE" scripts/build-games.sh "$@"
fi

CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}"
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"

# Panic locations (file:line:col) are compiled into the module — stable Rust has no flag
# to drop them — so the checkout, the registry and the toolchain's own sources all end up
# in it as paths. Remapping each to a fixed prefix is what lets two checkouts produce the
# same bytes. The same strings are why a change that only moves lines (cargo fmt
# included) changes a game's hash.
#
# std's own paths are /rustc/<commit>/library/... — unless the rust-src component is
# installed, in which case rustc points them at the local copy instead. The last remap
# folds that copy back to the canonical form, so the bytes do not depend on whether a
# machine has rust-src. (When several remaps match, rustc applies the last.)
SYSROOT="$(rustc --print sysroot)"
RUSTC_COMMIT="$(rustc -vV | sed -n 's/^commit-hash: //p')"
REMAP="--remap-path-prefix=$ROOT=/construct-game-sdk --remap-path-prefix=$CARGO_HOME=/cargo --remap-path-prefix=$SYSROOT=/rust --remap-path-prefix=$SYSROOT/lib/rustlib/src/rust=/rustc/$RUSTC_COMMIT"

# The host gives every call a fresh instance, so a module's initial memory is allocated
# and zeroed on every call. Rust's default 1 MiB stack made that 1 088 KiB per call for
# tic-tac-toe, most of the cost of a call. 128 KiB is the games' stack; the stack is
# placed first in memory, so a game that overflows it traps instead of overwriting data.
STACK="-C link-arg=-zstack-size=131072"

export RUSTFLAGS="$REMAP $STACK"

games=()
for manifest in games/*/Cargo.toml; do
    games+=("$(basename "$(dirname "$manifest")")")
done

packages=()
for game in "${games[@]}"; do packages+=(-p "$game"); done
cargo build --locked --profile wasm --target "$TARGET" "${packages[@]}"

mkdir -p dist
modules=()
for game in "${games[@]}"; do
    cp "$TARGET_DIR/$TARGET/wasm/$game.wasm" "dist/$game.wasm"
    modules+=("dist/$game.wasm")
done

python3 -I scripts/wasm_inspect.py "${modules[@]}"

# Two copies on one machine share these prefixes, so check-reproducible.sh cannot see a
# path that escaped the remapping above — it would only show as a different hash on
# another machine. A path to the toolchain did escape once, before the sysroot remap.
for prefix in "$ROOT" "$CARGO_HOME" "$SYSROOT"; do
    if grep -l -a -F "$prefix" "${modules[@]}"; then
        echo "a module above contains $prefix — add a --remap-path-prefix for it" >&2
        exit 1
    fi
done

# dist/SHA256SUMS names what this build put in dist/; the host's tests check the module
# they load against it, so they never run a module some other build left behind.
(cd dist && sha256sum "${games[@]/%/.wasm}") > dist/SHA256SUMS
if [ "$MODE" = "--native" ]; then
    echo "native build: dist/ is for development; its hashes are not the reference ones"
elif [ "$MODE" = "--update" ]; then
    cp dist/SHA256SUMS GAMES.sha256
    echo "GAMES.sha256 updated"
elif ! diff -u GAMES.sha256 dist/SHA256SUMS; then
    echo "A game's hash changed. If that is intended, run with --update and commit GAMES.sha256." >&2
    exit 1
fi

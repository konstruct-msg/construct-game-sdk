#!/usr/bin/env bash
# Build the games from two copies of the tree at two different paths and require the
# same bytes. A game's id is its hash: if the hash depended on where it was built, an
# auditor could not rebuild a signed game and get the id that was signed.
#
# Runs only on the reference host (x86_64 Linux) — in practice, in CI. Off it,
# build-games.sh would run each copy in an emulated x86_64 container, and on the owner's
# Mac rustc hung under QEMU twice in a row at the same crate (2026-10-07), with no error.
# CI is a real x86_64 machine, and its builds matched the container's.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
REFERENCE_HOST=x86_64-unknown-linux-gnu
if [ "$(rustc -vV 2>/dev/null | sed -n 's/^host: //p')" != "$REFERENCE_HOST" ]; then
    echo "check-reproducible.sh runs on $REFERENCE_HOST only; CI runs it on every push" >&2
    exit 1
fi

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

for copy in first second-copy-at-a-longer-path; do
    mkdir -p "$WORK/$copy"
    rsync -a --exclude target --exclude dist --exclude .git "$ROOT/" "$WORK/$copy/"
    (cd "$WORK/$copy" && scripts/build-games.sh --update >/dev/null)
done

if diff -u "$WORK/first/GAMES.sha256" "$WORK/second-copy-at-a-longer-path/GAMES.sha256"; then
    echo "reproducible:"
    cat "$WORK/first/GAMES.sha256"
else
    echo "the two builds differ" >&2
    exit 1
fi

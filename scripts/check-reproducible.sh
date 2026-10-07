#!/usr/bin/env bash
# Build the games from two copies of the tree at two different paths and require the
# same bytes. A game's id is its hash: if the hash depended on where it was built, an
# auditor could not rebuild a signed game and get the id that was signed.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
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

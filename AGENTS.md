# AGENTS.md — construct-game-sdk

Hard invariants for this repository. Design and stages:
`~/Code/construct-docs/decisions/games-are-wasm-modules-with-a-declarative-view.md` and
`~/Code/construct-docs/decisions/games-execution-plan.md` — read both before changing the
ABI or adding a game.

## What this is

Games for Konstruct's virtual room. A game is a WASM module with pure functions; the host
(`construct-games-host`, plan stage 2, not written yet) runs it inside `construct-core`.
The app draws the board from the `GameView` the module returns; a game has no UI code.

| Path | What |
|---|---|
| `crates/construct-game-abi/proto/construct_game_abi.proto` | **the** ABI: exports, messages, cell numbering |
| `crates/construct-game-abi` | Rust types generated from it (protox + prost, no `protoc`) |
| `crates/construct-game-sdk` | `trait Game`, `export_game!`, `bytes` — what a module's exports run |
| `games/<name>` | one game per crate; `tictactoe` is the reference |
| `GAMES.sha256` | the hash of every game — its id. Tracked on purpose |

## Invariants

- **A module imports nothing and has no start function.** No clock, no randomness, no
  host calls. Randomness comes only as a move by `PLAYER_CHANCE`.
  `scripts/wasm_inspect.py` fails a module that imports; the host will too.
- **No floating point in a game.** The host will refuse `f32`/`f64` instructions (stage 2).
- **No hash maps in a game** — `BTreeMap`/`BTreeSet`. Iteration order must depend only on
  the state.
- **State encoding is canonical**: whatever `decode` accepts re-encodes to the same bytes.
  Both players hash it after every move.
- **Turn order is the SDK's** (`bytes::checked_apply`). A game's `apply` is called only for
  the player its own `status` names.
- **Bytes, never JSON**, across the ABI.
- **The proto is the one authority.** Never hand-write a type that mirrors a message.

## Game ids are hashes

A game's id is the SHA-256 of its `.wasm`. So:

- `scripts/build-games.sh` fails when a hash differs from `GAMES.sha256`. A change that
  moves a hash is a different game: run `--update` and commit the new hashes in the same
  change, saying why.
- **Any edit to a game's source moves its hash — `cargo fmt` included.** Panic locations
  (file:line:col) are compiled in, and stable Rust cannot drop them. Expect it; do not
  "fix" it with nightly flags.
- Builds are reproducible across paths and machines: `build-games.sh` remaps every path
  (checkout, cargo registry, toolchain, std sources), and fails if anything under `$HOME`
  is left in a module. `scripts/check-reproducible.sh` builds from two paths and compares;
  CI compares a Linux build with the hashes committed from macOS.
- `rust-toolchain.toml` moves in lockstep with construct-core's, and moving it changes
  every hash.

## Checks before a commit

```bash
cargo fmt --all
cargo clippy --locked --all-targets -- -D warnings
cargo clippy --locked --release --target wasm32-unknown-unknown --workspace -- -D warnings
cargo test --locked
scripts/build-games.sh          # --update if a hash moved on purpose
```

A test that cannot fail is worse than none. A new rule check is done when breaking the rule
in the source makes a named test fail. The tic-tac-toe tree walk was checked this way: it
fails on a disabled win check and on a removed turn check.

## Commits

Conventional Commits. Never commit on `main`; topic branch, PR when asked.

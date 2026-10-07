# AGENTS.md — construct-game-sdk

Hard invariants for this repository. Design and stages:
`~/Code/construct-docs/decisions/games-are-wasm-modules-with-a-declarative-view.md` and
`~/Code/construct-docs/decisions/games-execution-plan.md` — read both before changing the
ABI or adding a game.

## What this is

Games for Konstruct's virtual room. A game is a WASM module with pure functions; the host
(`construct-games-host`) runs it, and will run inside `construct-core` (plan stage 6).
The app draws the board from the `GameView` the module returns; a game has no UI code.

| Path | What |
|---|---|
| `crates/construct-game-abi/proto/construct_game_abi.proto` | **the** ABI: exports, messages, cell numbering |
| `crates/construct-game-abi` | Rust types generated from it (protox + prost, no `protoc`) |
| `crates/construct-game-sdk` | `trait Game`, `export_game!`, `bytes` — what a module's exports run |
| `crates/construct-games-host` | loads and runs modules: load-time checks, a fresh instance per call, fuel, memory |
| `crates/construct-games-host/src/game_match.rs` | the match protocol both sides run: numbered messages, state hash, turn order, hash-chain rolls, resign and draw |
| `crates/construct-game-check` | the audit tool: plays random games against a module and holds it to the ABI; CI runs it on every game |
| `games/<name>` | one game per crate; `tictactoe` is the reference, `chess` on `cozy-chess` |
| `GAMES.sha256` | the hash of every game — its id. Tracked on purpose |

## Invariants

- **A module imports nothing and has no start function.** No clock, no randomness, no
  host calls. Randomness comes only as a move by `PLAYER_CHANCE`.
  `scripts/wasm_inspect.py` fails a module that imports; the host will too.
- **No floating point and no SIMD in a game.** The host refuses both at load.
- **Every host call is a fresh instance.** Nothing a module keeps survives to the next
  call, so there is no `cg_free` and a module may treat memory as an arena. Do not add
  instance reuse to save time: it is what makes hidden state between calls impossible.
- **wasmi runs with `portable-dispatch`.** Without it a module looping until it ran out
  of fuel overflowed the host's native stack and aborted the process. `hostile.rs` has
  the regression tests; keep them.
- **No hash maps in a game** — `BTreeMap`/`BTreeSet`. Iteration order must depend only on
  the state.
- **State encoding is canonical**: whatever `decode` accepts re-encodes to the same bytes.
  Both players hash it after every move.
- **Turn order is checked twice**: by the SDK (`bytes::checked_apply`) inside the module,
  and by the match against the game's `status` — a third-party module need not use the SDK.
- **A side reveals its chain value for roll `k` only once its own game waits for roll
  `k`.** Earlier, the other side would know a future roll while choosing its moves.
  `values_leave_only_for_rolls_the_game_has_asked_for` checks it after every single step;
  a check after full delivery missed a one-roll-early leak (2026-10-07).
- **Match agreement assumes eventual delivery.** The two-sides test ends with a full
  resend; without it a lost resignation left the sides disagreeing, which is the
  network's fault, not the protocol's. Clients resend `Match::sent()`.
- **Cell numbers are the game's own fixed frame** (chess: a1 = 0); `GameView.flipped`
  turns the board for a viewer. Never number cells from a player's side: who that player
  is (white or black) is decided by the seed.
- **Draws are automatic, never claimed** — repetition, fifty moves, insufficient material.
  Both clients must reach the same verdict from the state alone.
- **Every call stays under a tenth of the fuel limit** (10 M since 2026-10-07, set from
  chess's worst position, 218 legal moves, 757 k). `construct-game-check` fails a game
  that comes closer. Raising the limit to fit a game is a decision, not a fix.
- **Each check in `construct-game-check` has a test that plants its defect**
  (`tests/defects.rs`) and one that shows the unbroken games pass. A new check comes with
  both.
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
- **The hashes are the reference build's: x86_64 Linux, rustc 1.96.0, the image pinned
  by digest in `build-games.sh`.** Cargo mixes `rustc -vV`, which names the host, into
  every crate's metadata, and the metadata reaches the module's bytes. So a Mac and Linux
  build of the same source are different games (found 2026-10-07 when CI disagreed).
  **CI is the reference builder**: its "Build games" step compares with `GAMES.sha256`,
  prints the hashes, and uploads the modules as the `games` artifact.
- Locally, `build-games.sh` re-runs itself in the pinned image under emulation. On the
  owner's Mac rustc hung there twice at the same crate, with no error. For development,
  use `build-games.sh --native`: the tests run on its `dist/`, and `GAMES.sha256` is left
  alone. When a hash moves on purpose, take the new one from CI's log. Never commit
  hashes from a native macOS build.
- Within the reference environment builds do not depend on the path: every path is
  remapped (checkout, cargo registry, toolchain, std sources), and the build fails if one
  of those prefixes is left in a module. `scripts/check-reproducible.sh` builds from two
  paths and compares. It runs on x86_64 Linux only, which means in CI.
- `rust-toolchain.toml` moves in lockstep with construct-core's, and moving it changes
  every hash.

## Checks before a commit

```bash
cargo fmt --all
cargo clippy --locked --all-targets -- -D warnings
cargo clippy --locked --release --target wasm32-unknown-unknown --workspace --exclude construct-games-host --exclude construct-game-check -- -D warnings
scripts/build-games.sh --native # dist/ for the host tests; CI checks the reference hashes
cargo test --locked
cargo run --release -p construct-game-check -- --games 60 dist/*.wasm   # a minute; CI runs it
```

A test that cannot fail is worse than none. A new rule check is done when breaking the rule
in the source makes a named test fail. The tic-tac-toe tree walk was checked this way: it
fails on a disabled win check and on a removed turn check. So were the host's checks on
imports, floats, fuel and output size, and the match's checks on the state hash, chain
values, turn order, conflicting repeats and reveal timing. An unlimited fuel budget shows
up as a hang, not a failure, so run that mutation under a time limit.

## Commits

Conventional Commits. **Commits go straight to `main`** (owner, 2026-10-07): nothing consumes
this repo yet, and branches only slow the work. This ends when construct-core pins the host by
`rev` (plan stage 6) — from then on it is a topic branch and a PR, as in every other
construct-* repo. Push only when asked.

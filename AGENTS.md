# AGENTS.md — construct-game-sdk

Games for Konstruct's virtual room. A game is a WASM module of pure functions; the host
(`construct-games-host`) runs it and will run inside `construct-core` (plan stage 6). The app draws
the board from the `GameView` the module returns; a game has no UI code. If a rule here and your
plan disagree, the rule wins until a `decisions/` note says otherwise.

## Read first

| File | Why |
|---|---|
| `~/Code/construct-docs/decisions/rules-from-the-first-year.md` | why the rules exist; checklist before any new protocol feature |
| `~/Code/construct-docs/decisions/games-are-wasm-modules-with-a-declarative-view.md` | the design — before changing the ABI or adding a game |
| `~/Code/construct-docs/decisions/games-execution-plan.md` | stages; «Как продолжить» is the resume point |
| `crates/construct-game-abi/proto/construct_game_abi.proto` | **the** ABI: exports, messages, cell numbering |
| `crates/construct-games-host/src/game_match.rs` | the match protocol both sides run: numbered messages, state hash, turn order, hash-chain rolls, resign, draw |
| `crates/construct-game-sdk` · `crates/construct-game-abi` · `crates/construct-games-host` | `trait Game` + `export_game!` · types generated from the proto (protox + prost, no `protoc`) · loader: load-time checks, fresh instance per call, fuel, memory |
| `crates/construct-game-check` | audit tool: plays random games against a module; CI runs it on every game |
| `games/<name>` · `GAMES.sha256` | one crate per game (`tictactoe` reference, `chess` on `cozy-chess`) · hash of every game = its id, tracked on purpose |

## The Zen of Konstruct

```
Someone has solved it before. Read them first.
The core decides; the app executes.
A message names the state it means. Time is not an address.
Reproduce, then fix.
One meaning, one carrier.
Bytes stay bytes.
Measure, then build.
The effect is the proof, not the exit code.
A test that cannot fail is worse than no test.
Boring before new.
Temporary is permanent, unless something checks it.
What nothing reads is not sent.
Silence is the worst failure: a message that never appears raises no error.
```

Canonical source: `~/Code/construct-docs/decisions/rules-from-the-first-year.md`. Change it there first.

## Rules

1. **Prior art first.** Before a new match-protocol mechanism, write down how others solve it and why we deviate.
2. **The module and host decide, the app executes.** State, hash, turn order and verdict that both players must compute identically come from the module and `game_match.rs`, never from a client.
3. **A message names the state it means.** Match messages are numbered and carry the state hash; a fix that needs a timer or retry budget means the message lacks an address.
4. **Reproduce, then fix.** A disagreement between the sides gets a test that reproduces it first.
5. **One meaning, one carrier.** The proto is the one authority.
6. **Bytes stay bytes.** Bytes, never JSON, across the ABI.
7. **Measure, then build.** The fuel limit is set from the measured worst position, not a guess.
8. **The effect is the proof.** A check is done when breaking its rule in the source makes a named test fail.
9. **A test that cannot fail is worse than none.** Mutate the source and watch the test go red; run an unlimited-fuel mutation under a time limit (it hangs, it does not fail).
10. **Depth before breadth.** Games are a new surface: nothing here ships to users before 1:1 delivery and sessions are boring.
11. **A temporary exception has an end and a check.** The straight-to-main exception below ends at plan stage 6.
12. **Branch and PR; own worktree per session.** Never `checkout -- .`, `reset --hard` or `stash` in a shared tree.

## Invariants of this repo

- **A module imports nothing and has no start function.** No clock, no randomness, no host calls; randomness only as a move by `PLAYER_CHANCE`. `scripts/wasm_inspect.py` and the host both reject imports.
- **No floating point and no SIMD in a game.** The host refuses both at load.
- **Every host call is a fresh instance.** No `cg_free`; a module may treat memory as an arena. Never add instance reuse: it is what makes hidden state between calls impossible.
- **wasmi runs with `portable-dispatch`.** Without it a module looping until out of fuel overflowed the host's native stack and aborted the process; keep the `hostile.rs` regression tests.
- **No hash maps in a game** — `BTreeMap`/`BTreeSet`; iteration order must depend only on the state.
- **State encoding is canonical:** whatever `decode` accepts re-encodes to the same bytes. Both players hash it after every move.
- **Turn order is checked twice:** by the SDK (`bytes::checked_apply`) in the module, and by the match against the game's `status` — a third-party module need not use the SDK.
- **A side reveals its chain value for roll `k` only once its own game waits for roll `k`.** Earlier, the other side knows a future roll while choosing moves. `values_leave_only_for_rolls_the_game_has_asked_for` checks after every single step; a check after full delivery missed a one-roll-early leak (2026-10-07).
- **Match agreement assumes eventual delivery.** The two-sides test ends with a full resend; clients resend `Match::sent()`.
- **Cell numbers are the game's own fixed frame** (chess: a1 = 0); `GameView.flipped` turns the board for a viewer. Never number from a player's side — the seed decides who is white.
- **Draws are automatic, never claimed** (repetition, fifty moves, insufficient material); both clients reach the same verdict from the state alone.
- **Every call stays under a tenth of the fuel limit** (10 M, set from chess's worst position: 218 legal moves, 757 k). `construct-game-check` fails a game that comes closer. Raising the limit to fit a game is a decision, not a fix.
- **Each check in `construct-game-check` has a test that plants its defect** (`tests/defects.rs`) and one that shows unbroken games pass. A new check comes with both.
- **Never hand-write a type that mirrors a proto message.**
- **`rust-toolchain.toml` moves in lockstep with construct-core's,** and moving it changes every hash.

### Game ids are hashes

A game's id is the SHA-256 of its `.wasm`.

- `scripts/build-games.sh` fails when a hash differs from `GAMES.sha256`. A change that moves a hash is a different game: run `--update` and commit the new hashes in the same change, saying why.
- **Any edit to a game's source moves its hash — `cargo fmt` included** (panic locations file:line:col are compiled in). Do not "fix" it with nightly flags.
- **The hashes are the reference build's: x86_64 Linux, rustc 1.96.0, the image pinned by digest in `build-games.sh`.** Cargo mixes the host-naming `rustc -vV` into crate metadata, so a Mac and a Linux build of one source are different games (found 2026-10-07). **CI is the reference builder:** its "Build games" step compares with `GAMES.sha256`, prints the hashes and uploads the `games` artifact. Never commit hashes from a native macOS build; take a moved hash from CI's log.
- Locally `build-games.sh` re-runs in the pinned image under emulation; on the owner's Mac rustc hung there twice at the same crate, silently. For development use `build-games.sh --native`: tests run on its `dist/`, `GAMES.sha256` is left alone.
- Within the reference environment builds do not depend on the path (every path is remapped; the build fails if a prefix is left in a module). `scripts/check-reproducible.sh` builds from two paths and compares — x86_64 Linux only, i.e. in CI.

### Checks before a commit

```bash
cargo fmt --all
cargo clippy --locked --all-targets -- -D warnings
cargo clippy --locked --release --target wasm32-unknown-unknown --workspace --exclude construct-games-host --exclude construct-game-check -- -D warnings
scripts/build-games.sh --native # dist/ for the host tests; CI checks the reference hashes
cargo test --locked
cargo run --release -p construct-game-check -- --games 60 dist/*.wasm   # a minute; CI runs it
```

## Git and process

- Conventional Commits.
- **The owner commits straight to `main`** (standing exception, 2026-10-07): nothing consumes this repo yet. It ends when construct-core pins the host by `rev` (plan stage 6); from then on a topic branch and a PR, as in every other construct-* repo. Agents work on a topic branch and open a PR; never commit on `main`. Push only when asked.
- Check the branch and `git status` before every commit; other sessions share the tree. Never `checkout -- .`, `restore .`, `reset --hard` or `stash`; never rewrite pushed history.

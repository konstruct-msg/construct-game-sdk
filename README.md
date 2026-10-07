# construct-game-sdk

Games for Konstruct's virtual room, and the SDK to write them.

A game is a WebAssembly module of pure functions: it never sees the network, the clock or
the people playing. Konstruct runs the module and draws the board the game describes. Two
players run the same module, and the module's SHA-256 is the game's id.

## Writing a game

```rust
#![cfg_attr(target_arch = "wasm32", no_std)]
extern crate alloc;

use construct_game_sdk::{Game, export_game /* … */};

pub struct MyGame;
export_game!(MyGame);

impl Game for MyGame {
    type State = /* your Codec type */;
    type Move = /* … */;
    type Options = ();
    // init, apply, legal_moves, view, status
}
```

`games/tictactoe` is the complete reference. The contract — what each export does, how
cells are numbered, what a `GameView` holds — is
[`crates/construct-game-abi/proto/construct_game_abi.proto`](crates/construct-game-abi/proto/construct_game_abi.proto).

```bash
scripts/build-games.sh --native # dist/<game>.wasm for development, checked
scripts/build-games.sh          # the reference build; compares with GAMES.sha256 (CI)
cargo test                      # native, and the built modules through the host
scripts/check-reproducible.sh   # same bytes from two checkout paths (x86_64 Linux / CI)
cargo run --release -p construct-game-check -- dist/*.wasm   # the audit tool
```

## Status

ABI v1, the SDK, the host, the match protocol, tic-tac-toe as the reference game and chess
are in place. Not yet written: backgammon, go.

| Game | Module | Of which | A call through the host |
|---|---|---|---|
| tic-tac-toe | 32 KB | | ~17 µs |
| chess | 1.6 MB | 1.5 MB `cozy-chess` move tables | 0.15–0.25 ms; worst position 0.76 M fuel of 10 M |

Sizes are a reference point, not a limit. A call's cost is mostly the copy of the module's
data into the fresh instance every call gets. See the plan in the Konstruct docs
vault, `decisions/games-execution-plan.md`.

Game hashes come from a reference build on x86_64 Linux, which is CI. Off Linux,
`build-games.sh --native` builds for development.

## License

Apache-2.0. See `LICENSE` and `NOTICE`.

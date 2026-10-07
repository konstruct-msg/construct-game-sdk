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
cargo test                      # native, through the same byte-level functions the module runs
scripts/build-games.sh          # dist/<game>.wasm, checked, hashes compared with GAMES.sha256
scripts/check-reproducible.sh   # same bytes from two different checkout paths
```

## Status

ABI v1 and the SDK are in place, with tic-tac-toe as the reference game (32 KB). Not yet
written: the host (`construct-games-host`), the match protocol, chess, backgammon, go. See
the plan in the Konstruct docs vault, `decisions/games-execution-plan.md`.

## License

Apache-2.0. See `LICENSE` and `NOTICE`.

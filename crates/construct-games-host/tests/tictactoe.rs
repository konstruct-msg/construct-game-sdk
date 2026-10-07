//! The differential test: tic-tac-toe run natively (the SDK's byte-level functions over
//! the Rust types) and as the built module (through the host), side by side, comparing
//! every answer byte for byte. Anything the wasm build does differently — the compiler,
//! the allocator, the host's copying — shows up here as the first differing answer.
//!
//! Uses `dist/tictactoe.wasm`; run `scripts/build-games.sh` (or `--native`) first. The
//! module must be the one that build listed in `dist/SHA256SUMS`, so the test never runs
//! a module some other build left behind. Whether it is also the reference build named
//! in `GAMES.sha256` is `build-games.sh`'s check, not this one's: on a Mac the reference
//! build may not be available, and the test still has to run.

use std::path::Path;

use construct_game_abi::{Message, MoveList, SEED_LEN};
use construct_game_sdk::{PLAYER_0, PLAYER_1, PLAYER_CHANCE, bytes};
use construct_games_host::{GameModule, Limits};
use tictactoe::TicTacToe;

const GAMES: u32 = 10_000;

fn module() -> GameModule {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let wasm = std::fs::read(root.join("dist/tictactoe.wasm"))
        .expect("dist/tictactoe.wasm is missing — run scripts/build-games.sh");
    let module = GameModule::load(&wasm, Limits::default()).expect("tic-tac-toe loads");

    let listed = std::fs::read_to_string(root.join("dist/SHA256SUMS")).unwrap();
    let expected = listed
        .lines()
        .find_map(|line| line.strip_suffix("  tictactoe.wasm"))
        .expect("dist/SHA256SUMS lists tictactoe.wasm");
    assert_eq!(
        format!("{:?}", module.id()),
        expected,
        "dist/tictactoe.wasm is not what the last build produced — run scripts/build-games.sh",
    );
    module
}

/// xorshift64*: deterministic, so a failure names a game that can be replayed.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// Both sides answer one export; the answers must be the same bytes.
fn same(
    module: &GameModule,
    export: &str,
    inputs: &[&[u8]],
    player: Option<u32>,
    native: Option<Vec<u8>>,
    game: u32,
) -> Vec<u8> {
    let native = native.unwrap_or_else(|| panic!("game {game}: native {export} failed"));
    let wasm = module
        .call(export, inputs, player)
        .unwrap_or_else(|e| panic!("game {game}: wasm {export} failed: {e}"));
    assert_eq!(wasm.bytes, native, "game {game}: {export} differs");
    native
}

fn state_of(apply_result: &[u8]) -> Option<Vec<u8>> {
    use construct_game_abi::{ApplyResult, apply_result::Result};
    match ApplyResult::decode(apply_result).unwrap().result {
        Some(Result::State(state)) => Some(state),
        _ => None,
    }
}

#[test]
fn the_module_answers_exactly_what_the_native_code_answers() {
    let module = module();
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut max_fuel = 0;

    for game in 0..GAMES {
        let mut seed = [0u8; SEED_LEN];
        seed.iter_mut().for_each(|b| *b = rng.next() as u8);
        let init = same(
            &module,
            "cg_init",
            &[&seed, &[]],
            None,
            bytes::init::<TicTacToe>(&seed, &[]),
            game,
        );
        let mut state = state_of(&init).expect("init gives a state");

        loop {
            let st: &[u8] = &state;
            same(
                &module,
                "cg_status",
                &[st],
                None,
                bytes::status::<TicTacToe>(st),
                game,
            );
            let mut offered = Vec::new();
            for player in [PLAYER_0, PLAYER_1] {
                let list = same(
                    &module,
                    "cg_legal_moves",
                    &[st],
                    Some(player),
                    bytes::legal_moves::<TicTacToe>(st, player),
                    game,
                );
                same(
                    &module,
                    "cg_view",
                    &[st],
                    Some(player),
                    bytes::view::<TicTacToe>(st, player),
                    game,
                );
                let moves = MoveList::decode(list.as_slice()).unwrap().moves;
                if !moves.is_empty() {
                    offered.push((player, moves));
                }
            }

            // Now and then a move nobody may make, so refusals are compared too.
            let (player, mv) = match rng.below(8) {
                0 => (PLAYER_CHANCE, vec![rng.next() as u8]),
                1 => (rng.below(2) as u32, vec![rng.below(12) as u8]),
                _ => match offered.first() {
                    Some((player, moves)) => {
                        (*player, moves[rng.below(moves.len())].r#move.clone())
                    }
                    None => break, // finished
                },
            };
            let result = same(
                &module,
                "cg_apply",
                &[st, &mv],
                Some(player),
                bytes::apply::<TicTacToe>(st, &mv, player),
                game,
            );
            max_fuel = max_fuel.max(
                module
                    .call("cg_apply", &[st, &mv], Some(player))
                    .unwrap()
                    .fuel,
            );
            if let Some(next) = state_of(&result) {
                state = next;
            }
        }
    }
    eprintln!("{GAMES} games; most fuel for one apply: {max_fuel}");
}

/// A state the module never produced is a module failure on both sides, not a guess.
#[test]
fn a_corrupted_state_fails_on_both_sides() {
    let module = module();
    let state = [7u8; 11];
    assert!(bytes::status::<TicTacToe>(&state).is_none());
    assert_eq!(
        module.status(&state).unwrap_err(),
        construct_games_host::CallError::ModuleFailed,
    );
}

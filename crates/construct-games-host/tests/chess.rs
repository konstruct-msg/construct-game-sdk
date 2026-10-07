//! Chess through the built module. The full perft runs natively (`games/chess`); here it
//! runs as deep as the cost of a call allows — each call copies the module's 1.5 MB of
//! move tables into a fresh instance — through positions that between them hold
//! castling, en passant and promotion. Then random games, every answer of the module
//! byte-equal to the native code's.

mod common;

use std::path::Path;

use chess::{Chess, State};
use common::Rng;
use construct_game_abi::{Message, MoveList, SEED_LEN, Status, status};
use construct_game_sdk::{Codec, bytes};
use construct_games_host::{Applied, GameModule, Limits};

fn module() -> GameModule {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let wasm = std::fs::read(root.join("dist/chess.wasm"))
        .expect("dist/chess.wasm is missing — run scripts/build-games.sh --native");
    GameModule::load(&wasm, Limits::default()).expect("chess loads")
}

fn to_move(module: &GameModule, state: &[u8]) -> Option<u32> {
    match module.status(state).unwrap().state {
        Some(status::State::ToMove(p)) => Some(p),
        _ => None,
    }
}

fn perft(module: &GameModule, state: &[u8], depth: u32) -> u64 {
    let Some(player) = to_move(module, state) else {
        return 0;
    };
    let moves = module.legal_moves(state, player).unwrap().moves;
    if depth == 1 {
        return moves.len() as u64;
    }
    moves
        .iter()
        .map(|m| match module.apply(state, &m.r#move, player).unwrap() {
            Applied::State(next) => perft(module, &next, depth - 1),
            Applied::Invalid(invalid) => panic!("an offered move was refused: {invalid:?}"),
        })
        .sum()
}

#[test]
fn perft_through_the_module() {
    let module = module();
    for (fen, depth, nodes) in [
        (
            "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1",
            3,
            8_902,
        ),
        (
            "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1",
            2,
            2_039,
        ),
        (
            "r3k2r/Pppp1ppp/1b3nbN/nP6/BBP1P3/q4N2/Pp1P2PP/R2Q1RK1 w kq - 0 1",
            2,
            264,
        ),
        (
            "rnbq1k1r/pp1Pbppp/2p5/8/2B5/8/PPP1NnPP/RNBQK2R w KQ - 1 8",
            2,
            1_486,
        ),
    ] {
        let state = State::from_fen(fen, 0).unwrap().encode();
        assert_eq!(
            perft(&module, &state, depth),
            nodes,
            "{fen} at depth {depth}"
        );
    }
}

#[test]
fn random_games_answer_byte_for_byte_as_the_native_code() {
    let module = module();
    let mut rng = Rng(0xC4E5_5000_0000_0001);
    let mut most_fuel = 0;
    for game in 0..40 {
        let seed: [u8; SEED_LEN] = rng.bytes();
        let native = bytes::init::<Chess>(&seed, &[]).unwrap();
        let wasm = module.call("cg_init", &[&seed, &[]], None).unwrap();
        assert_eq!(wasm.bytes, native, "game {game}: init");
        let Applied::State(mut state) = Applied::decode(&native).unwrap() else {
            panic!()
        };

        for ply in 0..160 {
            let same =
                |export: &str, inputs: &[&[u8]], player: Option<u32>, native: Option<Vec<u8>>| {
                    let wasm = module.call(export, inputs, player).unwrap();
                    assert_eq!(
                        Some(wasm.bytes.clone()),
                        native,
                        "game {game}, ply {ply}: {export}"
                    );
                    wasm
                };
            let status = same("cg_status", &[&state], None, bytes::status::<Chess>(&state)).bytes;
            let Some(status::State::ToMove(player)) =
                Status::decode(status.as_slice()).unwrap().state
            else {
                break;
            };
            for viewer in [0, 1] {
                same(
                    "cg_view",
                    &[&state],
                    Some(viewer),
                    bytes::view::<Chess>(&state, viewer),
                );
            }
            let list = same(
                "cg_legal_moves",
                &[&state],
                Some(player),
                bytes::legal_moves::<Chess>(&state, player),
            );
            most_fuel = most_fuel.max(list.fuel);
            let moves = MoveList::decode(list.bytes.as_slice()).unwrap().moves;
            let mv = moves[rng.below(moves.len())].r#move.clone();
            let applied = same(
                "cg_apply",
                &[&state, &mv],
                Some(player),
                bytes::apply::<Chess>(&state, &mv, player),
            );
            most_fuel = most_fuel.max(applied.fuel);
            let Applied::State(next) = Applied::decode(&applied.bytes).unwrap() else {
                panic!()
            };
            state = next;
        }
    }
    eprintln!("most fuel for one call: {most_fuel}");
}

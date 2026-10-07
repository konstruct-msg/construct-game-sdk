//! Every test goes through `construct_game_sdk::bytes` — the functions the module's
//! exports call — so what is checked here is what a host will run.

use construct_game_sdk::abi::{
    ApplyResult, GameView, Message, MoveList, Status, apply_result, outcome, status,
};
use construct_game_sdk::{PLAYER_0, PLAYER_1, PLAYER_CHANCE, bytes, invalid};

use super::*;

/// Distinct complete games of tic-tac-toe, a long-known figure: a rules error that lets
/// a game run past a win, or stop early, moves it.
const COMPLETE_GAMES: u64 = 255_168;

fn seed(first: u8) -> [u8; SEED_LEN] {
    let mut seed = [0u8; SEED_LEN];
    seed[0] = first;
    seed
}

fn start(first: u8) -> Vec<u8> {
    state_of(bytes::init::<TicTacToe>(&seed(first), &[]).expect("init answers"))
}

fn state_of(result: Vec<u8>) -> Vec<u8> {
    match ApplyResult::decode(result.as_slice()).unwrap().result {
        Some(apply_result::Result::State(state)) => state,
        other => panic!("expected a state, got {other:?}"),
    }
}

fn invalid_code(result: Vec<u8>) -> u32 {
    match ApplyResult::decode(result.as_slice()).unwrap().result {
        Some(apply_result::Result::Invalid(invalid)) => invalid.code,
        other => panic!("expected invalid, got {other:?}"),
    }
}

fn status_of(state: &[u8]) -> status::State {
    let bytes = bytes::status::<TicTacToe>(state).expect("status answers");
    Status::decode(bytes.as_slice())
        .unwrap()
        .state
        .expect("status set")
}

fn legal(state: &[u8], player: u32) -> MoveList {
    MoveList::decode(
        bytes::legal_moves::<TicTacToe>(state, player)
            .unwrap()
            .as_slice(),
    )
    .unwrap()
}

fn apply(state: &[u8], cell: u8, player: u32) -> Vec<u8> {
    bytes::apply::<TicTacToe>(state, &[cell], player).expect("apply answers")
}

/// Walks every game from `state` and checks, at every position, everything the ABI
/// promises about it. Returns the number of complete games below.
fn walk(state: &[u8]) -> u64 {
    let decoded = State::decode(state).expect("every reached state decodes");
    assert_eq!(decoded.encode(), state, "encoding is canonical");

    let to_move = match status_of(state) {
        status::State::ToMove(p) => p,
        status::State::Finished(_) => {
            for p in [PLAYER_0, PLAYER_1] {
                assert!(legal(state, p).moves.is_empty());
                assert_eq!(invalid_code(apply(state, 0, p)), invalid::FINISHED);
            }
            return 1;
        }
        status::State::AwaitingChance(_) => panic!("tic-tac-toe never asks for chance"),
    };
    let other = 1 - to_move;

    assert!(
        legal(state, other).moves.is_empty(),
        "only the player to move has moves"
    );
    assert_eq!(invalid_code(apply(state, 0, other)), invalid::NOT_YOUR_TURN);
    assert_eq!(
        invalid_code(bytes::apply::<TicTacToe>(state, &[0], PLAYER_CHANCE).unwrap()),
        invalid::NOT_YOUR_TURN,
    );

    let view = bytes::view::<TicTacToe>(state, to_move).unwrap();
    let view = GameView::decode(view.as_slice()).unwrap();
    assert_eq!(
        view.pieces.len(),
        decoded.cells.iter().filter(|&&c| c != 0).count()
    );

    let moves = legal(state, to_move).moves;
    let mut games = 0;
    for cell in 0..CELLS as u8 {
        let offered = moves.iter().find(|m| m.to == Some(u32::from(cell)));
        if let Some(offered) = offered {
            assert_eq!(offered.r#move, vec![cell]);
            games += walk(&state_of(apply(state, cell, to_move)));
        } else {
            assert_eq!(invalid_code(apply(state, cell, to_move)), INVALID_OCCUPIED);
        }
    }
    games
}

#[test]
fn every_game_from_either_first_player_is_legal_and_ends() {
    assert_eq!(walk(&start(0)), COMPLETE_GAMES);
    assert_eq!(walk(&start(1)), COMPLETE_GAMES);
}

#[test]
fn the_seed_decides_who_moves_first() {
    assert_eq!(status_of(&start(0)), status::State::ToMove(PLAYER_0));
    assert_eq!(status_of(&start(1)), status::State::ToMove(PLAYER_1));
    // Only the low bit counts, so every seed picks someone.
    assert_eq!(status_of(&start(0xFF)), status::State::ToMove(PLAYER_1));
}

#[test]
fn a_line_wins_for_its_owner() {
    // Player 0 takes the bottom row while player 1 plays the middle one.
    let mut state = start(0);
    for (cell, player) in [
        (0, PLAYER_0),
        (3, PLAYER_1),
        (1, PLAYER_0),
        (4, PLAYER_1),
        (2, PLAYER_0),
    ] {
        state = state_of(apply(&state, cell, player));
    }
    match status_of(&state) {
        status::State::Finished(outcome) => {
            assert_eq!(outcome.result, Some(outcome::Result::Winner(PLAYER_0)));
        }
        other => panic!("expected a finished game, got {other:?}"),
    }
    let view = GameView::decode(
        bytes::view::<TicTacToe>(&state, PLAYER_1)
            .unwrap()
            .as_slice(),
    )
    .unwrap();
    assert_eq!(view.status.unwrap().key, "game.tictactoe.status.lost");
    let line: Vec<u32> = view
        .highlights
        .iter()
        .filter(|h| h.kind == i32::from(highlight::Kind::WinningLine))
        .map(|h| h.cell)
        .collect();
    assert_eq!(line, vec![0, 1, 2]);
}

#[test]
fn a_move_that_is_not_a_cell_is_refused_as_encoding() {
    let state = start(0);
    for mv in [&[9u8][..], &[], &[0, 1]] {
        let result = bytes::apply::<TicTacToe>(&state, mv, PLAYER_0).unwrap();
        assert_eq!(invalid_code(result), invalid::MOVE_ENCODING, "move {mv:?}");
    }
}

#[test]
fn options_must_be_empty() {
    let result = bytes::init::<TicTacToe>(&seed(0), &[1]).unwrap();
    assert_eq!(invalid_code(result), invalid::OPTIONS);
}

/// Bytes the host never got from this module are a module failure, not an answer.
#[test]
fn a_bad_seed_or_state_fails_the_module() {
    assert!(bytes::init::<TicTacToe>(&[0; SEED_LEN - 1], &[]).is_none());

    let mut wrong_length = start(0);
    wrong_length.push(0);
    let mut bad_cell = start(0);
    bad_cell[0] = 3;
    let mut bad_first = start(0);
    bad_first[CELLS] = 2;
    let mut two_ahead = start(0);
    two_ahead[0] = 1;
    two_ahead[1] = 1;
    two_ahead[CELLS + 1] = 1;
    let mut last_on_empty = start(0);
    last_on_empty[CELLS + 1] = 4;

    for state in [wrong_length, bad_cell, bad_first, two_ahead, last_on_empty] {
        assert!(
            bytes::status::<TicTacToe>(&state).is_none(),
            "state {state:?}"
        );
        assert!(bytes::apply::<TicTacToe>(&state, &[0], PLAYER_0).is_none());
    }
}

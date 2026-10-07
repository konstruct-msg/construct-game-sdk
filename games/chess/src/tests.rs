//! Chess through `construct_game_sdk::bytes` — the functions the module's exports run.
//! Perft counts are the published ones (chessprogramming.org, "Perft Results"): a move
//! generator that differs from them anywhere differs at some depth.

use construct_game_sdk::abi::{
    ApplyResult, GameView, Message, MoveList, Status, apply_result, highlight, outcome, status,
};
use construct_game_sdk::{PLAYER_0, PLAYER_1, SEED_LEN, bytes, invalid};

use super::*;

const START: &str = "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1";
const KIWIPETE: &str = "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1";
const POSITION_3: &str = "8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 0 1";
const POSITION_4: &str = "r3k2r/Pppp1ppp/1b3nbN/nP6/BBP1P3/q4N2/Pp1P2PP/R2Q1RK1 w kq - 0 1";
const POSITION_5: &str = "rnbq1k1r/pp1Pbppp/2p5/8/2B5/8/PPP1NnPP/RNBQK2R w KQ - 1 8";

/// White is player 0 in every hand-made state below.
fn state(fen: &str) -> Vec<u8> {
    State::from_fen(fen, 0).expect("valid FEN").encode()
}

fn to_move(state: &[u8]) -> u32 {
    match status_of(state) {
        status::State::ToMove(p) => p,
        other => panic!("nobody to move: {other:?}"),
    }
}

fn status_of(state: &[u8]) -> status::State {
    Status::decode(bytes::status::<Chess>(state).unwrap().as_slice())
        .unwrap()
        .state
        .unwrap()
}

fn moves(state: &[u8]) -> MoveList {
    let player = to_move(state);
    MoveList::decode(
        bytes::legal_moves::<Chess>(state, player)
            .unwrap()
            .as_slice(),
    )
    .unwrap()
}

fn apply(state: &[u8], mv: &[u8]) -> Result<Vec<u8>, u32> {
    let player = to_move(state);
    let result = bytes::apply::<Chess>(state, mv, player).expect("apply answers");
    match ApplyResult::decode(result.as_slice())
        .unwrap()
        .result
        .unwrap()
    {
        apply_result::Result::State(next) => Ok(next),
        apply_result::Result::Invalid(invalid) => Err(invalid.code),
    }
}

fn play(state: &[u8], from: Square, to: Square, promotion: u8) -> Vec<u8> {
    apply(state, &[from as u8, to as u8, promotion]).expect("legal")
}

fn board_of(state: &[u8]) -> Board {
    State::decode(state).unwrap().board
}

fn finished(state: &[u8]) -> (Option<u32>, String) {
    match status_of(state) {
        status::State::Finished(o) => {
            let winner = match o.result {
                Some(outcome::Result::Winner(w)) => Some(w),
                _ => None,
            };
            (winner, o.reason_key)
        }
        other => panic!("not finished: {other:?}"),
    }
}

/// Leaves at `depth`, counting the last ply from the move list without applying it.
/// Every move offered must apply; no two may share bytes.
fn perft(state: &[u8], depth: u32) -> u64 {
    // Mate and stalemate: no moves, nothing below. (At these depths no position reaches a
    // draw that perft would still count moves in — repetition takes 8 plies.)
    if !matches!(status_of(state), status::State::ToMove(_)) {
        return 0;
    }
    let list = moves(state).moves;
    let mut seen: Vec<&[u8]> = list.iter().map(|m| m.r#move.as_slice()).collect();
    seen.sort();
    seen.dedup();
    assert_eq!(seen.len(), list.len(), "two moves with the same bytes");
    if depth == 1 {
        return list.len() as u64;
    }
    list.iter()
        .map(|m| {
            perft(
                &apply(state, &m.r#move).expect("offered moves apply"),
                depth - 1,
            )
        })
        .sum()
}

#[test]
fn perft_start() {
    let start = state(START);
    for (depth, nodes) in [(1, 20), (2, 400), (3, 8_902), (4, 197_281), (5, 4_865_609)] {
        assert_eq!(perft(&start, depth), nodes, "depth {depth}");
    }
}

#[test]
fn perft_kiwipete() {
    let s = state(KIWIPETE);
    for (depth, nodes) in [(1, 48), (2, 2_039), (3, 97_862), (4, 4_085_603)] {
        assert_eq!(perft(&s, depth), nodes, "depth {depth}");
    }
}

#[test]
fn perft_positions_3_4_5() {
    let p3 = state(POSITION_3);
    for (depth, nodes) in [(1, 14), (2, 191), (3, 2_812), (4, 43_238), (5, 674_624)] {
        assert_eq!(perft(&p3, depth), nodes, "position 3, depth {depth}");
    }
    let p4 = state(POSITION_4);
    for (depth, nodes) in [(1, 6), (2, 264), (3, 9_467), (4, 422_333)] {
        assert_eq!(perft(&p4, depth), nodes, "position 4, depth {depth}");
    }
    let p5 = state(POSITION_5);
    for (depth, nodes) in [(1, 44), (2, 1_486), (3, 62_379), (4, 2_103_487)] {
        assert_eq!(perft(&p5, depth), nodes, "position 5, depth {depth}");
    }
}

// ── single rules ─────────────────────────────────────────────────────────────────

#[test]
fn the_seed_decides_who_plays_white() {
    for (bit, white) in [(0u8, PLAYER_0), (1, PLAYER_1)] {
        let mut seed = [0u8; SEED_LEN];
        seed[0] = bit;
        let result = bytes::init::<Chess>(&seed, &[]).unwrap();
        let Some(apply_result::Result::State(s)) =
            ApplyResult::decode(result.as_slice()).unwrap().result
        else {
            panic!()
        };
        assert_eq!(to_move(&s), white, "white moves first");
    }
}

#[test]
fn checkmate_ends_the_game_for_the_mating_side() {
    let s = play(
        &state("6k1/5ppp/8/8/8/8/5PPP/R5K1 w - - 0 1"),
        Square::A1,
        Square::A8,
        0,
    );
    assert_eq!(
        finished(&s),
        (Some(PLAYER_0), "game.chess.end.checkmate".into())
    );
}

#[test]
fn stalemate_is_a_draw() {
    let s = state("7k/5Q2/6K1/8/8/8/8/8 b - - 0 1");
    assert_eq!(finished(&s), (None, "game.chess.end.stalemate".into()));
}

#[test]
fn a_pawn_on_the_last_rank_offers_four_promotions() {
    let s = state("4k3/P7/8/8/8/8/8/4K3 w - - 0 1");
    let from_a7: Vec<_> = moves(&s)
        .moves
        .into_iter()
        .filter(|m| m.from == Some(Square::A7 as u32))
        .collect();
    let mut choices: Vec<_> = from_a7.iter().map(|m| m.choice_key.as_str()).collect();
    choices.sort();
    assert_eq!(
        choices,
        [
            "game.chess.promote.bishop",
            "game.chess.promote.knight",
            "game.chess.promote.queen",
            "game.chess.promote.rook"
        ],
    );
    let queened = play(&s, Square::A7, Square::A8, 4);
    assert_eq!(board_of(&queened).piece_on(Square::A8), Some(Piece::Queen));
    let knighted = play(&s, Square::A7, Square::A8, 1);
    assert_eq!(
        board_of(&knighted).piece_on(Square::A8),
        Some(Piece::Knight)
    );
    assert_eq!(
        apply(&s, &[Square::A7 as u8, Square::A8 as u8, 0]),
        Err(INVALID_ILLEGAL),
        "promotion is not optional"
    );
}

#[test]
fn en_passant_takes_the_pawn_beside() {
    let s = play(
        &state("4k3/8/8/3pP3/8/8/8/4K3 w - d6 0 1"),
        Square::E5,
        Square::D6,
        0,
    );
    let board = board_of(&s);
    assert_eq!(board.piece_on(Square::D6), Some(Piece::Pawn));
    assert_eq!(board.piece_on(Square::D5), None);
}

#[test]
fn castling_is_the_king_moving_two_squares_and_not_through_an_attacked_square() {
    let s = state("4k3/8/8/8/8/8/5r2/R3K2R w KQ - 0 1");
    let king: Vec<_> = moves(&s)
        .moves
        .into_iter()
        .filter(|m| m.from == Some(Square::E1 as u32))
        .map(|m| m.to.unwrap())
        .collect();
    assert!(
        king.contains(&(Square::C1 as u32)),
        "long castling, nothing in the way"
    );
    assert!(
        !king.contains(&(Square::G1 as u32)),
        "short castling would cross f1, attacked by the rook"
    );
    assert!(
        !king.contains(&(Square::H1 as u32)) && !king.contains(&(Square::A1 as u32)),
        "no king-takes-rook spelling"
    );
    let castled = board_of(&play(&s, Square::E1, Square::C1, 0));
    assert_eq!(castled.piece_on(Square::C1), Some(Piece::King));
    assert_eq!(castled.piece_on(Square::D1), Some(Piece::Rook));
}

#[test]
fn a_third_repetition_is_a_draw() {
    let mut s = state(START);
    let shuffle = [
        (Square::G1, Square::F3),
        (Square::G8, Square::F6),
        (Square::F3, Square::G1),
        (Square::F6, Square::G8),
    ];
    for round in 0..2 {
        for (i, &(from, to)) in shuffle.iter().enumerate() {
            assert!(
                matches!(status_of(&s), status::State::ToMove(_)),
                "round {round}, ply {i}: still playing"
            );
            s = play(&s, from, to, 0);
        }
    }
    assert_eq!(finished(&s), (None, "game.chess.end.repetition".into()));
}

/// An en passant square nobody can capture on does not make a position new.
#[test]
fn a_dead_en_passant_square_does_not_count_for_repetition() {
    let dead_ep: Board = "rnbqkbnr/pppppppp/8/8/4P3/8/PPPP1PPP/RNBQKBNR b KQkq e3 0 1"
        .parse()
        .unwrap();
    let no_ep: Board = "rnbqkbnr/pppppppp/8/8/4P3/8/PPPP1PPP/RNBQKBNR b KQkq - 4 3"
        .parse()
        .unwrap();
    assert_eq!(repetition_key(&dead_ep), repetition_key(&no_ep));
    let live_ep: Board = "rnbqkb1r/ppp1pppp/5n2/3pP3/8/8/PPPP1PPP/RNBQKBNR w KQkq d6 0 3"
        .parse()
        .unwrap();
    let gone: Board = "rnbqkb1r/ppp1pppp/5n2/3pP3/8/8/PPPP1PPP/RNBQKBNR w KQkq - 4 5"
        .parse()
        .unwrap();
    assert_ne!(repetition_key(&live_ep), repetition_key(&gone));
}

#[test]
fn the_hundredth_quiet_half_move_is_a_draw_unless_it_mates() {
    let s = play(
        &state("4k3/8/8/8/8/8/8/R3K3 w - - 99 80"),
        Square::A1,
        Square::A2,
        0,
    );
    assert_eq!(finished(&s), (None, "game.chess.end.fifty_moves".into()));
    let mate = play(
        &state("6k1/5ppp/8/8/8/8/5PPP/R5K1 w - - 99 80"),
        Square::A1,
        Square::A8,
        0,
    );
    assert_eq!(
        finished(&mate),
        (Some(PLAYER_0), "game.chess.end.checkmate".into())
    );
}

#[test]
fn material_nobody_can_mate_with_is_a_draw() {
    for fen in [
        "4k3/8/8/8/8/8/8/4K3 w - - 0 1",
        "4k3/8/8/8/8/8/8/4K1N1 w - - 0 1",
        "4k3/8/8/8/8/8/8/2B1K3 w - - 0 1",
        "4k3/8/8/8/8/8/7b/2B1K3 w - - 0 1", // both bishops on dark squares
    ] {
        assert_eq!(
            finished(&state(fen)),
            (None, "game.chess.end.insufficient_material".into()),
            "{fen}"
        );
    }
    for fen in [
        "4k3/8/8/8/8/8/8/2B1K2b w - - 0 1", // bishops on opposite colours
        "4k3/8/8/8/8/8/8/1N2K1N1 w - - 0 1",
        "4k3/8/8/8/8/8/P7/4K3 w - - 0 1",
    ] {
        assert!(
            matches!(status_of(&state(fen)), status::State::ToMove(_)),
            "{fen}"
        );
    }
}

#[test]
fn a_capture_or_pawn_move_starts_the_history_again() {
    let s = play(
        &play(&state(START), Square::G1, Square::F3, 0),
        Square::E7,
        Square::E5,
        0,
    );
    assert_eq!(State::decode(&s).unwrap().history.len(), 1);
}

#[test]
fn moves_that_are_not_moves_are_refused() {
    let s = state(START);
    assert_eq!(
        apply(&s, &[Square::E2 as u8, Square::E5 as u8, 0]),
        Err(INVALID_ILLEGAL)
    );
    for bad in [&[12u8, 28][..], &[64, 0, 0], &[12, 28, 5], &[12, 28, 0, 0]] {
        assert_eq!(apply(&s, bad), Err(invalid::MOVE_ENCODING), "{bad:?}");
    }
}

// ── encoding and view ────────────────────────────────────────────────────────────

/// Walks random games and checks every state re-encodes to itself and decodes to
/// itself; then refuses states the encoding cannot produce.
#[test]
fn the_state_encoding_is_canonical() {
    let mut rng = 0x1234_5678_u64;
    let mut next = || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng
    };
    for _ in 0..30 {
        let mut s = state(START);
        for _ in 0..120 {
            let decoded = State::decode(&s).expect("decodes");
            assert_eq!(decoded.encode(), s);
            if !matches!(status_of(&s), status::State::ToMove(_)) {
                break;
            }
            let list = moves(&s).moves;
            s = apply(&s, &list[(next() % list.len() as u64) as usize].r#move).unwrap();
        }
    }

    let good = state(START);
    let mut wrong_key = good.clone();
    *wrong_key.last_mut().unwrap() ^= 1;
    let mut too_long = good.clone();
    let fen_len = usize::from(good[3]);
    too_long[4 + fen_len] = 2;
    too_long.extend_from_slice(&[0; 8]);
    let mut other_spelling = good.clone();
    let fen = START.replace(" 0 1", " 00 1");
    other_spelling.splice(
        3..4 + fen_len,
        [&[fen.len() as u8][..], fen.as_bytes()].concat(),
    );
    for bad in [wrong_key, too_long, other_spelling] {
        assert!(State::decode(&bad).is_none());
    }
}

#[test]
fn the_board_is_flipped_for_black_and_check_is_shown() {
    let s = state("4k3/8/8/8/8/8/4r3/4K3 w - - 0 1");
    let white = GameView::decode(bytes::view::<Chess>(&s, PLAYER_0).unwrap().as_slice()).unwrap();
    let black = GameView::decode(bytes::view::<Chess>(&s, PLAYER_1).unwrap().as_slice()).unwrap();
    assert!(!white.flipped);
    assert!(black.flipped);
    assert_eq!(
        white.status.unwrap().key,
        "game.chess.status.your_turn_check"
    );
    assert_eq!(black.status.unwrap().key, "game.chess.status.their_turn");
    assert!(
        white
            .highlights
            .iter()
            .any(|h| h.cell == Square::E1 as u32 && h.kind == i32::from(highlight::Kind::Check))
    );
    let king = white
        .pieces
        .iter()
        .find(|p| p.cell == Square::E1 as u32)
        .unwrap();
    assert_eq!((king.image.as_str(), king.owner), ("white_king", PLAYER_0));
}

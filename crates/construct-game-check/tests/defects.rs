//! Each defect the tool looks for, planted once, must be found. A checker that passes
//! everything is worse than none: these are its own tests.
//!
//! Games run natively, behind `Target`, through the SDK's byte functions — the code a
//! module's exports run. A defect is planted either in a game (a `Game` that breaks one
//! rule) or in the target (an answer rewritten on the way out, for what the SDK would not
//! let a game get wrong). Every test first shows the unbroken game passes.

use std::cell::Cell;
use std::marker::PhantomData;

use construct_game_abi::{
    ApplyResult, ChanceRoll, ChanceSpec, GameView, Message, MoveList, Piece, Status, apply_result,
    outcome, status,
};
use construct_game_check::{Config, Kind, Report, Target, check};
use construct_game_sdk::{Codec, Game, Invalid, LegalMove, Outcome, SEED_LEN, bytes};
use construct_games_host::{CallError, Output};
use tictactoe::TicTacToe;

/// A game's native code as a `Target`, fuel 0.
struct Native<G>(PhantomData<G>);

impl<G> Native<G> {
    fn new() -> Self {
        Self(PhantomData)
    }
}

impl<G: Game> Target for Native<G> {
    fn call(
        &self,
        export: &str,
        inputs: &[&[u8]],
        player: Option<u32>,
    ) -> Result<Output, CallError> {
        let p = player.unwrap_or(0);
        let bytes = match export {
            "cg_init" => bytes::init::<G>(inputs[0], inputs[1]),
            "cg_apply" => bytes::apply::<G>(inputs[0], inputs[1], p),
            "cg_legal_moves" => bytes::legal_moves::<G>(inputs[0], p),
            "cg_view" => bytes::view::<G>(inputs[0], p),
            "cg_status" => bytes::status::<G>(inputs[0]),
            _ => unreachable!("{export}"),
        };
        bytes
            .map(|bytes| Output { bytes, fuel: 0 })
            .ok_or(CallError::ModuleFailed)
    }
}

/// A target whose answers pass through `edit` on the way out.
struct Sabotaged<T, F> {
    inner: T,
    edit: F,
}

impl<T: Target, F> Target for Sabotaged<T, F>
where
    F: Fn(&T, &str, &[&[u8]], Option<u32>, Output) -> Output,
{
    fn call(
        &self,
        export: &str,
        inputs: &[&[u8]],
        player: Option<u32>,
    ) -> Result<Output, CallError> {
        let output = self.inner.call(export, inputs, player)?;
        Ok((self.edit)(&self.inner, export, inputs, player, output))
    }
}

fn sabotaged<T: Target, F>(inner: T, edit: F) -> Sabotaged<T, F>
where
    F: Fn(&T, &str, &[&[u8]], Option<u32>, Output) -> Output,
{
    Sabotaged { inner, edit }
}

fn config() -> Config {
    Config {
        games: 30,
        full_games: 5,
        ..Config::default()
    }
}

fn found(report: &Report, kind: impl Fn(&Kind) -> bool) {
    assert!(report.has(&kind), "not found; report:\n{report}");
}

fn moves_of(output: &Output) -> Vec<construct_game_abi::MoveOption> {
    MoveList::decode(output.bytes.as_slice()).unwrap().moves
}

fn with_moves(moves: Vec<construct_game_abi::MoveOption>) -> Output {
    Output {
        bytes: MoveList { moves }.encode_to_vec(),
        fuel: 0,
    }
}

fn to_move(target: &impl Target, state: &[u8]) -> Option<u32> {
    let bytes = target.call("cg_status", &[state], None).unwrap().bytes;
    match Status::decode(bytes.as_slice()).unwrap().state {
        Some(status::State::ToMove(p)) => Some(p),
        _ => None,
    }
}

// ── the controls ─────────────────────────────────────────────────────────────────

#[test]
fn the_unbroken_games_pass() {
    let report = check(&Native::<TicTacToe>::new(), &config());
    assert!(report.passed(), "{report}");
    assert_eq!(report.finished, 30);
    let report = check(&Native::<Race>::new(), &config());
    assert!(report.passed(), "{report}");
}

// ── defects in a game ────────────────────────────────────────────────────────────

/// Tic-tac-toe that offers every cell, taken or not.
struct OffersTakenCells;

impl Game for OffersTakenCells {
    type State = <TicTacToe as Game>::State;
    type Move = <TicTacToe as Game>::Move;
    type Options = ();
    fn init(seed: &[u8; SEED_LEN], o: ()) -> Result<Self::State, Invalid> {
        TicTacToe::init(seed, o)
    }
    fn apply(s: &Self::State, m: &Self::Move, p: u32) -> Result<Self::State, Invalid> {
        TicTacToe::apply(s, m, p)
    }
    fn legal_moves(s: &Self::State, p: u32) -> Vec<LegalMove<Self::Move>> {
        if TicTacToe::legal_moves(s, p).is_empty() {
            return Vec::new();
        }
        (0..9u8)
            .map(|c| LegalMove::to(Self::Move::decode(&[c]).unwrap(), u32::from(c)))
            .collect()
    }
    fn view(s: &Self::State, p: u32) -> GameView {
        TicTacToe::view(s, p)
    }
    fn status(s: &Self::State) -> Status {
        TicTacToe::status(s)
    }
}

#[test]
fn an_offered_move_the_game_refuses_is_found() {
    found(&check(&Native::<OffersTakenCells>::new(), &config()), |k| {
        matches!(k, Kind::OfferedMoveRefused { .. })
    });
}

/// Tic-tac-toe whose move is any bytes: the first byte, mod 9, onto any cell, taken or not.
struct TakesAnything;

#[derive(Clone, Copy)]
struct AnyCell(u8);

impl Codec for AnyCell {
    fn encode(&self) -> Vec<u8> {
        vec![self.0]
    }
    fn decode(bytes: &[u8]) -> Option<Self> {
        Some(Self(bytes.first().copied().unwrap_or(0) % 9))
    }
}

impl Game for TakesAnything {
    type State = <TicTacToe as Game>::State;
    type Move = AnyCell;
    type Options = ();
    fn init(seed: &[u8; SEED_LEN], o: ()) -> Result<Self::State, Invalid> {
        TicTacToe::init(seed, o)
    }
    fn apply(s: &Self::State, m: &AnyCell, p: u32) -> Result<Self::State, Invalid> {
        let free = TicTacToe::legal_moves(s, p).into_iter().next().unwrap().mv;
        let wanted = <TicTacToe as Game>::Move::decode(&[m.0]).unwrap();
        TicTacToe::apply(s, &wanted, p).or_else(|_| TicTacToe::apply(s, &free, p))
    }
    fn legal_moves(s: &Self::State, p: u32) -> Vec<LegalMove<AnyCell>> {
        TicTacToe::legal_moves(s, p)
            .into_iter()
            .map(|m| LegalMove::to(AnyCell(m.to.unwrap() as u8), m.to.unwrap()))
            .collect()
    }
    fn view(s: &Self::State, p: u32) -> GameView {
        TicTacToe::view(s, p)
    }
    fn status(s: &Self::State) -> Status {
        TicTacToe::status(s)
    }
}

#[test]
fn a_move_never_offered_that_the_game_accepts_is_found() {
    found(&check(&Native::<TakesAnything>::new(), &config()), |k| {
        matches!(k, Kind::UnofferedMoveAccepted { .. })
    });
}

// ── defects in the answers ───────────────────────────────────────────────────────

#[test]
fn a_move_by_the_other_player_accepted_is_found() {
    // Played as if by the player to move, whoever sent it.
    let target = sabotaged(
        Native::<TicTacToe>::new(),
        |inner, export, inputs, player, output| {
            if export != "cg_apply" || player == Some(255) {
                return output;
            }
            match to_move(inner, inputs[0]) {
                Some(mover) if player != Some(mover) => {
                    inner.call(export, inputs, Some(mover)).unwrap()
                }
                _ => output,
            }
        },
    );
    found(&check(&target, &config()), |k| {
        *k == Kind::OtherPlayerAccepted
    });
}

#[test]
fn a_move_by_chance_accepted_is_found() {
    let target = sabotaged(
        Native::<TicTacToe>::new(),
        |inner, export, inputs, player, output| {
            if export == "cg_apply" && player == Some(255) {
                inner
                    .call(export, inputs, to_move(inner, inputs[0]))
                    .unwrap()
            } else {
                output
            }
        },
    );
    found(&check(&target, &config()), |k| {
        *k == Kind::ChanceMoveAccepted
    });
}

#[test]
fn no_moves_for_the_player_to_move_is_found() {
    let target = sabotaged(Native::<TicTacToe>::new(), |_, export, _, _, output| {
        if export == "cg_legal_moves" {
            with_moves(Vec::new())
        } else {
            output
        }
    });
    found(&check(&target, &config()), |k| {
        *k == Kind::NoMovesForPlayerToMove
    });
}

#[test]
fn moves_for_the_player_not_to_move_are_found() {
    let target = sabotaged(
        Native::<TicTacToe>::new(),
        |inner, export, inputs, _, output| {
            if export != "cg_legal_moves" {
                return output;
            }
            match to_move(inner, inputs[0]) {
                Some(mover) => inner.call(export, inputs, Some(mover)).unwrap(),
                None => output,
            }
        },
    );
    found(&check(&target, &config()), |k| {
        matches!(k, Kind::MovesForPlayerNotToMove { .. })
    });
}

#[test]
fn a_move_offered_twice_is_found() {
    let target = sabotaged(Native::<TicTacToe>::new(), |_, export, _, _, output| {
        if export != "cg_legal_moves" {
            return output;
        }
        let mut moves = moves_of(&output);
        if let Some(first) = moves.first().cloned() {
            moves.push(first);
        }
        with_moves(moves)
    });
    found(&check(&target, &config()), |k| {
        matches!(k, Kind::DuplicateMove { .. })
    });
}

#[test]
fn a_move_with_no_cell_and_no_button_is_found() {
    let target = sabotaged(Native::<TicTacToe>::new(), |_, export, _, _, output| {
        if export != "cg_legal_moves" {
            return output;
        }
        with_moves(
            moves_of(&output)
                .into_iter()
                .map(|m| construct_game_abi::MoveOption { to: None, ..m })
                .collect(),
        )
    });
    found(&check(&target, &config()), |k| {
        matches!(k, Kind::MoveWithoutTarget { .. })
    });
}

#[test]
fn moves_on_the_same_cells_without_a_choice_are_found() {
    let target = sabotaged(Native::<TicTacToe>::new(), |_, export, _, _, output| {
        if export != "cg_legal_moves" {
            return output;
        }
        let mut moves = moves_of(&output);
        if let Some(first) = moves.first().cloned() {
            let mut twin = first;
            twin.r#move.push(0);
            moves.push(twin);
        }
        with_moves(moves)
    });
    found(&check(&target, &config()), |k| {
        matches!(k, Kind::AmbiguousChoice { .. })
    });
}

#[test]
fn a_move_onto_a_cell_off_the_board_is_found() {
    let target = sabotaged(Native::<TicTacToe>::new(), |_, export, _, _, output| {
        if export != "cg_legal_moves" {
            return output;
        }
        with_moves(
            moves_of(&output)
                .into_iter()
                .map(|m| construct_game_abi::MoveOption { to: Some(9), ..m })
                .collect(),
        )
    });
    found(&check(&target, &config()), |k| {
        matches!(
            k,
            Kind::CellOffBoard {
                what: "move",
                cell: 9,
                cells: 9
            }
        )
    });
}

#[test]
fn a_piece_off_the_board_is_found() {
    let target = sabotaged(Native::<TicTacToe>::new(), |_, export, _, _, output| {
        if export != "cg_view" {
            return output;
        }
        let mut view = GameView::decode(output.bytes.as_slice()).unwrap();
        view.pieces.push(Piece {
            cell: 40,
            ..Piece::default()
        });
        Output {
            bytes: view.encode_to_vec(),
            fuel: 0,
        }
    });
    found(&check(&target, &config()), |k| {
        matches!(
            k,
            Kind::CellOffBoard {
                what: "piece",
                cell: 40,
                ..
            }
        )
    });
}

#[test]
fn a_view_without_a_board_or_status_is_found() {
    for (strip_board, expected) in [(true, Kind::NoBoard), (false, Kind::NoStatusText)] {
        let target = sabotaged(
            Native::<TicTacToe>::new(),
            move |_, export, _, _, output| {
                if export != "cg_view" {
                    return output;
                }
                let mut view = GameView::decode(output.bytes.as_slice()).unwrap();
                if strip_board {
                    view.board = None
                } else {
                    view.status = None
                }
                Output {
                    bytes: view.encode_to_vec(),
                    fuel: 0,
                }
            },
        );
        found(&check(&target, &config()), |k| *k == expected);
    }
}

#[test]
fn an_answer_that_does_not_decode_is_found() {
    let target = sabotaged(Native::<TicTacToe>::new(), |_, export, _, _, output| {
        if export == "cg_status" {
            Output {
                bytes: vec![0xFF, 0xFF],
                fuel: 0,
            }
        } else {
            output
        }
    });
    found(&check(&target, &config()), |k| {
        *k == Kind::Malformed { what: "Status" }
    });
}

/// Every seventh apply answers with the state another offered move would have made — a
/// valid state, and a different one each time the count lands elsewhere: the same
/// question, a different answer, as hidden state between calls would give.
#[test]
fn a_different_state_on_replay_is_found() {
    let calls = Cell::new(0u32);
    let target = sabotaged(
        Native::<TicTacToe>::new(),
        move |inner, export, inputs, player, output| {
            if export != "cg_apply" || player == Some(255) {
                return output;
            }
            calls.set(calls.get() + 1);
            let applied = matches!(
                ApplyResult::decode(output.bytes.as_slice()).unwrap().result,
                Some(apply_result::Result::State(_))
            );
            if !calls.get().is_multiple_of(7) || !applied {
                return output;
            }
            let offers = moves_of(&inner.call("cg_legal_moves", &[inputs[0]], player).unwrap());
            match offers.iter().find(|m| m.r#move != inputs[1]) {
                Some(other) => inner
                    .call(export, &[inputs[0], &other.r#move], player)
                    .unwrap(),
                None => output,
            }
        },
    );
    found(&check(&target, &config()), |k| *k == Kind::Nondeterministic);
}

#[test]
fn fuel_closer_than_a_tenth_of_the_limit_is_found() {
    let target = sabotaged(Native::<TicTacToe>::new(), |_, export, _, _, mut output| {
        if export == "cg_view" {
            output.fuel = Config::default().fuel_limit / 5;
        }
        output
    });
    found(&check(&target, &config()), |k| {
        matches!(
            k,
            Kind::FuelMargin {
                export: "cg_view",
                ..
            }
        )
    });
}

#[test]
fn options_the_game_refuses_stop_the_check() {
    let config = Config {
        options: vec![1],
        ..config()
    };
    let report = check(&Native::<TicTacToe>::new(), &config);
    found(&report, |k| matches!(k, Kind::InitRefused { .. }));
    assert_eq!(report.games, 1, "nothing else can be checked");
}

// ── rolls ────────────────────────────────────────────────────────────────────────

/// A race to 20 on one die; every turn starts with a roll.
struct Race;

#[derive(Clone)]
struct RaceState([u8; 4]); // positions, to move, roll (0: awaiting one)

impl Codec for RaceState {
    fn encode(&self) -> Vec<u8> {
        self.0.to_vec()
    }
    fn decode(b: &[u8]) -> Option<Self> {
        let s: [u8; 4] = b.try_into().ok()?;
        (s[2] <= 1 && s[3] <= 6).then_some(Self(s))
    }
}

impl Game for Race {
    type State = RaceState;
    type Move = ();
    type Options = ();
    fn init(seed: &[u8; SEED_LEN], _: ()) -> Result<RaceState, Invalid> {
        Ok(RaceState([0, 0, seed[0] & 1, 0]))
    }
    fn apply(s: &RaceState, _: &(), p: u32) -> Result<RaceState, Invalid> {
        let mut n = s.0;
        n[p as usize] += n[3];
        n[2] = 1 - n[2];
        n[3] = 0;
        Ok(RaceState(n))
    }
    fn apply_chance(s: &RaceState, roll: &ChanceRoll) -> Result<RaceState, Invalid> {
        let mut n = s.0;
        n[3] = roll.values[0] as u8;
        Ok(RaceState(n))
    }
    fn legal_moves(s: &RaceState, p: u32) -> Vec<LegalMove<()>> {
        match Self::status(s).state {
            Some(status::State::ToMove(m)) if m == p => vec![LegalMove::action((), "game.race.go")],
            _ => Vec::new(),
        }
    }
    fn view(_: &RaceState, _: u32) -> GameView {
        GameView {
            board: Some(construct_game_abi::Board {
                kind: Some(construct_game_abi::board::Kind::Grid(
                    construct_game_abi::Grid {
                        width: 20,
                        height: 2,
                    },
                )),
            }),
            status: Some(construct_game_abi::Text {
                key: "game.race.status".into(),
                args: Vec::new(),
            }),
            ..GameView::default()
        }
    }
    fn status(s: &RaceState) -> Status {
        let [a, b, to_move, roll] = s.0;
        let state = if a >= 20 || b >= 20 {
            let result = if a >= 20 {
                outcome::Result::Winner(0)
            } else {
                outcome::Result::Winner(1)
            };
            status::State::Finished(Outcome {
                result: Some(result),
                reason_key: "game.race.end".into(),
            })
        } else if roll == 0 {
            status::State::AwaitingChance(ChanceSpec { dice: vec![6] })
        } else {
            status::State::ToMove(u32::from(to_move))
        };
        Status { state: Some(state) }
    }
}

#[test]
fn a_roll_outside_the_spec_accepted_is_found() {
    // Any roll at all is answered as if it were a 3.
    let target = sabotaged(
        Native::<Race>::new(),
        |inner, export, inputs, player, output| {
            if export == "cg_apply" && player == Some(255) {
                let three = ChanceRoll { values: vec![3] }.encode_to_vec();
                inner.call(export, &[inputs[0], &three], player).unwrap()
            } else {
                output
            }
        },
    );
    found(&check(&target, &config()), |k| {
        matches!(k, Kind::BadRollAccepted { .. })
    });
}

#[test]
fn a_roll_inside_the_spec_refused_is_found() {
    let target = sabotaged(
        Native::<Race>::new(),
        |_, export, inputs, player, output| {
            let six = ChanceRoll { values: vec![6] }.encode_to_vec();
            if export == "cg_apply" && player == Some(255) && inputs[1] == six.as_slice() {
                let refused = construct_game_abi::Invalid {
                    code: 1,
                    reason_key: "nope".into(),
                };
                Output {
                    bytes: ApplyResult {
                        result: Some(apply_result::Result::Invalid(refused)),
                    }
                    .encode_to_vec(),
                    fuel: 0,
                }
            } else {
                output
            }
        },
    );
    found(
        &check(&target, &config()),
        |k| matches!(k, Kind::RollRefused { values, .. } if values == &[6]),
    );
}

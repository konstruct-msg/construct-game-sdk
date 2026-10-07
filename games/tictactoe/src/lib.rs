//! Tic-tac-toe — the reference game for the ABI.
//!
//! Small enough that its whole game tree is walked in a test, which is what makes it the
//! place to check the SDK and, later, the host: anything that goes wrong here goes wrong
//! in the ABI, not in the rules.

#![cfg_attr(target_arch = "wasm32", no_std)]

extern crate alloc;

use alloc::string::ToString;
use alloc::vec;
use alloc::vec::Vec;

use construct_game_sdk::abi::{Board, Draw, Grid, board, highlight, outcome, status};
use construct_game_sdk::{
    Codec, Game, GameView, Highlight, Invalid, LegalMove, Outcome, PLAYER_1, Piece, SEED_LEN,
    Status, Text, export_game,
};

pub struct TicTacToe;

export_game!(TicTacToe);

const SIZE: u32 = 3;
const CELLS: usize = 9;
const NO_MOVE: u8 = u8::MAX;
const STATE_LEN: usize = CELLS + 2;

const LINES: [[usize; 3]; 8] = [
    [0, 1, 2],
    [3, 4, 5],
    [6, 7, 8],
    [0, 3, 6],
    [1, 4, 7],
    [2, 5, 8],
    [0, 4, 8],
    [2, 4, 6],
];

pub const INVALID_OCCUPIED: u32 = 1;

/// Cells hold 0 for empty or `player + 1`. Index = y * 3 + x (see `Board` in the ABI).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct State {
    cells: [u8; CELLS],
    /// The player who moved first and plays X.
    first: u8,
    last: u8,
}

impl State {
    fn count(&self, player: u32) -> usize {
        self.cells
            .iter()
            .filter(|&&c| u32::from(c) == player + 1)
            .count()
    }

    fn to_move(&self) -> u32 {
        let first = u32::from(self.first);
        if self.count(first) == self.count(1 - first) {
            first
        } else {
            1 - first
        }
    }

    fn winning_line(&self) -> Option<[usize; 3]> {
        LINES.into_iter().find(|line| {
            let c = self.cells[line[0]];
            c != 0 && line.iter().all(|&i| self.cells[i] == c)
        })
    }

    fn is_full(&self) -> bool {
        self.cells.iter().all(|&c| c != 0)
    }
}

/// Eleven bytes: the cells, `first`, `last`. Canonical because `encode` is a plain copy of
/// the fields: whatever `decode` accepts re-encodes to the same bytes. `decode` also
/// rejects counts no game can reach, so a corrupted state fails as a module error rather
/// than playing on.
impl Codec for State {
    fn encode(&self) -> Vec<u8> {
        let mut bytes = self.cells.to_vec();
        bytes.push(self.first);
        bytes.push(self.last);
        bytes
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        let bytes: &[u8; STATE_LEN] = bytes.try_into().ok()?;
        let state = State {
            cells: bytes[..CELLS].try_into().ok()?,
            first: bytes[CELLS],
            last: bytes[CELLS + 1],
        };
        let first = u32::from(state.first);
        let reachable = first <= PLAYER_1
            && state.cells.iter().all(|&c| c <= 2)
            && (state.count(first) == state.count(1 - first)
                || state.count(first) == state.count(1 - first) + 1)
            && match state.last {
                NO_MOVE => state.cells == [0; CELLS],
                last => state.cells.get(usize::from(last)).is_some_and(|&c| c != 0),
            };
        reachable.then_some(state)
    }
}

/// One byte, the cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Move(u8);

impl Codec for Move {
    fn encode(&self) -> Vec<u8> {
        vec![self.0]
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        match bytes {
            [cell] if usize::from(*cell) < CELLS => Some(Move(*cell)),
            _ => None,
        }
    }
}

fn text(key: &str) -> Text {
    Text {
        key: key.to_string(),
        args: Vec::new(),
    }
}

impl Game for TicTacToe {
    type State = State;
    type Move = Move;
    type Options = ();

    fn init(seed: &[u8; SEED_LEN], _options: ()) -> Result<State, Invalid> {
        Ok(State {
            cells: [0; CELLS],
            first: seed[0] & 1,
            last: NO_MOVE,
        })
    }

    fn apply(state: &State, mv: &Move, player: u32) -> Result<State, Invalid> {
        let cell = usize::from(mv.0);
        if state.cells[cell] != 0 {
            return Err(Invalid {
                code: INVALID_OCCUPIED,
                reason_key: "game.tictactoe.invalid.occupied".to_string(),
            });
        }
        let mut next = state.clone();
        next.cells[cell] = player as u8 + 1;
        next.last = mv.0;
        Ok(next)
    }

    fn legal_moves(state: &State, player: u32) -> Vec<LegalMove<Move>> {
        let ongoing =
            matches!(Self::status(state).state, Some(status::State::ToMove(p)) if p == player);
        if !ongoing {
            return Vec::new();
        }
        (0..CELLS as u8)
            .filter(|&cell| state.cells[usize::from(cell)] == 0)
            .map(|cell| LegalMove::to(Move(cell), u32::from(cell)))
            .collect()
    }

    fn view(state: &State, player: u32) -> GameView {
        let first = u32::from(state.first);
        let pieces = (0..CELLS)
            .filter(|&i| state.cells[i] != 0)
            .map(|i| {
                let owner = u32::from(state.cells[i]) - 1;
                let mark = if owner == first { "x" } else { "o" };
                Piece {
                    cell: i as u32,
                    image: mark.to_string(),
                    owner,
                    count: 1,
                    label_key: ["game.tictactoe.piece.", mark].concat(),
                }
            })
            .collect();

        let mut highlights = Vec::new();
        if state.last != NO_MOVE {
            highlights.push(Highlight {
                cell: u32::from(state.last),
                kind: highlight::Kind::LastMove.into(),
            });
        }
        if let Some(line) = state.winning_line() {
            highlights.extend(line.iter().map(|&i| Highlight {
                cell: i as u32,
                kind: highlight::Kind::WinningLine.into(),
            }));
        }

        let status = match Self::status(state).state {
            Some(status::State::ToMove(p)) if p == player => {
                text("game.tictactoe.status.your_turn")
            }
            Some(status::State::ToMove(_)) => text("game.tictactoe.status.their_turn"),
            Some(status::State::Finished(Outcome {
                result: Some(outcome::Result::Winner(w)),
                ..
            })) => text(if w == player {
                "game.tictactoe.status.won"
            } else {
                "game.tictactoe.status.lost"
            }),
            _ => text("game.tictactoe.status.draw"),
        };

        GameView {
            board: Some(Board {
                kind: Some(board::Kind::Grid(Grid {
                    width: SIZE,
                    height: SIZE,
                })),
            }),
            pieces,
            highlights,
            status: Some(status),
            flipped: false,
        }
    }

    fn status(state: &State) -> Status {
        let finished = |result, key: &str| {
            status::State::Finished(Outcome {
                result: Some(result),
                reason_key: key.to_string(),
            })
        };
        let state = if let Some(line) = state.winning_line() {
            let winner = u32::from(state.cells[line[0]]) - 1;
            finished(outcome::Result::Winner(winner), "game.tictactoe.end.line")
        } else if state.is_full() {
            finished(outcome::Result::Draw(Draw {}), "game.tictactoe.end.full")
        } else {
            status::State::ToMove(state.to_move())
        };
        Status { state: Some(state) }
    }
}

#[cfg(test)]
mod tests;

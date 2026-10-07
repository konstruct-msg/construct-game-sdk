//! Chess. Move generation and legality are `cozy-chess`; this crate adds what a match
//! between two clients needs on top: who plays white, every automatic draw (repetition,
//! fifty moves, insufficient material), the ABI's encodings and the view.
//!
//! Draws are automatic, not claimed: a client has nobody to claim them from, and both
//! sides must reach the same verdict from the state alone.

#![cfg_attr(target_arch = "wasm32", no_std)]

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use construct_game_sdk::abi::{Board as AbiBoard, Draw, Grid, board, highlight, outcome, status};
use construct_game_sdk::{
    Codec, Game, GameView, Highlight, Invalid, LegalMove, Outcome, Piece as AbiPiece, SEED_LEN,
    Status, Text, export_game,
};
use cozy_chess::{BitBoard, Board, Color, File, GameStatus, Move, Piece, Rank, Square};

pub struct Chess;

export_game!(Chess);

pub const INVALID_ILLEGAL: u32 = 1;

const NO_SQUARE: u8 = u8::MAX;
/// No FEN is longer: 64 squares + 7 slashes + side, castling, ep, two counters.
const MAX_FEN: usize = 100;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct State {
    board: Board,
    /// The player who plays white.
    white: u8,
    /// The last move, for the view; `NO_SQUARE` before the first.
    last: (u8, u8),
    /// Repetition keys of every position since the last capture or pawn move, the current
    /// one last. A capture or pawn move makes every earlier position unreachable, which is
    /// what keeps this as short as the fifty-move counter.
    history: Vec<u64>,
}

impl State {
    pub fn new(board: Board, white: u8) -> Self {
        let history = vec![repetition_key(&board)];
        Self {
            board,
            white,
            last: (NO_SQUARE, NO_SQUARE),
            history,
        }
    }

    /// For tests and tools: a state from a FEN, `white` playing white.
    pub fn from_fen(fen: &str, white: u8) -> Option<Self> {
        Some(Self::new(fen.parse().ok()?, white))
    }

    pub fn board(&self) -> &Board {
        &self.board
    }

    fn player(&self, color: Color) -> u32 {
        let white = u32::from(self.white);
        if color == Color::White {
            white
        } else {
            1 - white
        }
    }

    fn color(&self, player: u32) -> Color {
        if player == u32::from(self.white) {
            Color::White
        } else {
            Color::Black
        }
    }

    fn repetitions(&self) -> usize {
        let current = *self.history.last().expect("never empty");
        self.history.iter().filter(|&&key| key == current).count()
    }
}

/// `[white, last from, last to, FEN length, FEN…, history length, keys (u64 LE)…]`.
/// Canonical because `decode` re-encodes what it parsed and refuses bytes that differ —
/// a FEN has more than one spelling of the same board, and only `cozy-chess`'s own is
/// accepted.
impl Codec for State {
    fn encode(&self) -> Vec<u8> {
        let fen = format!("{}", self.board);
        let mut bytes = vec![self.white, self.last.0, self.last.1, fen.len() as u8];
        bytes.extend_from_slice(fen.as_bytes());
        bytes.push(self.history.len() as u8);
        for key in &self.history {
            bytes.extend_from_slice(&key.to_le_bytes());
        }
        bytes
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        let (&[white, from, to, fen_len], rest) = bytes.split_first_chunk::<4>()?;
        let fen_len = usize::from(fen_len);
        if white > 1 || fen_len > MAX_FEN || rest.len() < fen_len + 1 {
            return None;
        }
        let board: Board = core::str::from_utf8(&rest[..fen_len]).ok()?.parse().ok()?;
        let count = usize::from(rest[fen_len]);
        let keys = &rest[fen_len + 1..];
        if count == 0 || keys.len() != count * 8 || count > usize::from(board.halfmove_clock()) + 1
        {
            return None;
        }
        let history: Vec<u64> = keys
            .chunks_exact(8)
            .map(|k| u64::from_le_bytes(k.try_into().unwrap()))
            .collect();
        let last_ok = |s: u8| s == NO_SQUARE || s < 64;
        if !last_ok(from) || !last_ok(to) || (from == NO_SQUARE) != (to == NO_SQUARE) {
            return None;
        }
        if *history.last()? != repetition_key(&board) {
            return None;
        }
        let state = Self {
            board,
            white,
            last: (from, to),
            history,
        };
        (state.encode() == bytes).then_some(state)
    }
}

/// A move as players think of it: from, to, promotion (0 none, 1 knight … 4 queen).
/// Castling is the king moving two squares, not `cozy-chess`'s king-takes-rook.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChessMove {
    pub from: u8,
    pub to: u8,
    pub promotion: u8,
}

impl Codec for ChessMove {
    fn encode(&self) -> Vec<u8> {
        vec![self.from, self.to, self.promotion]
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        match *bytes {
            [from, to, promotion] if from < 64 && to < 64 && promotion <= 4 => Some(Self {
                from,
                to,
                promotion,
            }),
            _ => None,
        }
    }
}

const PROMOTIONS: [Piece; 4] = [Piece::Knight, Piece::Bishop, Piece::Rook, Piece::Queen];

/// The player's view of a `cozy-chess` move.
fn player_move(board: &Board, mv: Move) -> ChessMove {
    let king = board.piece_on(mv.from) == Some(Piece::King);
    let own_rook = board.color_on(mv.to) == board.color_on(mv.from);
    let to = if king && own_rook {
        let file = if mv.to.file() > mv.from.file() {
            File::G
        } else {
            File::C
        };
        Square::new(file, mv.from.rank())
    } else {
        mv.to
    };
    let promotion = mv.promotion.map_or(0, |p| {
        PROMOTIONS.iter().position(|&q| q == p).unwrap() as u8 + 1
    });
    ChessMove {
        from: mv.from as u8,
        to: to as u8,
        promotion,
    }
}

fn legal(board: &Board) -> Vec<Move> {
    let mut moves = Vec::new();
    board.generate_moves(|piece_moves| {
        moves.extend(piece_moves);
        false
    });
    moves
}

/// The position's identity for repetition, as FIDE counts it: the board, the side to
/// move, castling rights, and an en passant square only if a capture on it is legal.
/// `cozy-chess`'s own hash includes the en passant file whenever a pawn has just moved
/// two squares, capturable or not, and would miss repetitions.
fn repetition_key(board: &Board) -> u64 {
    let ep_square = board
        .en_passant()
        .map(|file| Square::new(file, Rank::Sixth.relative_to(board.side_to_move())));
    let ep_live = ep_square.is_some_and(|ep| {
        legal(board)
            .iter()
            .any(|mv| mv.to == ep && board.piece_on(mv.from) == Some(Piece::Pawn))
    });
    let ep = if ep_live {
        ep_square.map_or(0, |s| s.file() as u64 + 1)
    } else {
        0
    };
    board.hash_without_ep() ^ ep.wrapping_mul(0x9E37_79B9_7F4A_7C15)
}

/// Neither side can ever mate: kings alone, one minor piece, or bishops only, all on
/// squares of one colour.
fn insufficient_material(board: &Board) -> bool {
    let kings = board.pieces(Piece::King);
    let others = board.occupied() & !kings;
    let bishops = board.pieces(Piece::Bishop);
    let minors = bishops | board.pieces(Piece::Knight);
    others.is_empty()
        || (others.len() == 1 && others.is_subset(minors))
        || (others.is_subset(bishops)
            && (others.is_subset(BitBoard::LIGHT_SQUARES)
                || others.is_subset(BitBoard::DARK_SQUARES)))
}

fn text(key: &str) -> Text {
    Text {
        key: key.to_string(),
        args: Vec::new(),
    }
}

fn piece_word(piece: Piece) -> &'static str {
    match piece {
        Piece::Pawn => "pawn",
        Piece::Knight => "knight",
        Piece::Bishop => "bishop",
        Piece::Rook => "rook",
        Piece::Queen => "queen",
        Piece::King => "king",
    }
}

/// `white_knight`: the image name, and the tail of the piece's label key.
fn piece_name(color: Color, piece: Piece) -> String {
    let color = if color == Color::White {
        "white"
    } else {
        "black"
    };
    format!("{color}_{}", piece_word(piece))
}

impl Game for Chess {
    type State = State;
    type Move = ChessMove;
    type Options = ();

    fn init(seed: &[u8; SEED_LEN], _: ()) -> Result<State, Invalid> {
        Ok(State::new(Board::default(), seed[0] & 1))
    }

    fn apply(state: &State, mv: &ChessMove, player: u32) -> Result<State, Invalid> {
        // The SDK calls this only for the player `status` names, who has the move.
        let _ = player;
        let board = &state.board;
        let Some(chosen) = legal(board)
            .into_iter()
            .find(|&m| player_move(board, m) == *mv)
        else {
            return Err(Invalid {
                code: INVALID_ILLEGAL,
                reason_key: "game.chess.invalid.illegal".to_string(),
            });
        };
        let mut next = board.clone();
        next.play_unchecked(chosen);
        let mut history = if next.halfmove_clock() == 0 {
            Vec::new()
        } else {
            state.history.clone()
        };
        history.push(repetition_key(&next));
        Ok(State {
            board: next,
            white: state.white,
            last: (mv.from, mv.to),
            history,
        })
    }

    fn legal_moves(state: &State, player: u32) -> Vec<LegalMove<ChessMove>> {
        let ongoing =
            matches!(Self::status(state).state, Some(status::State::ToMove(p)) if p == player);
        if !ongoing {
            return Vec::new();
        }
        legal(&state.board)
            .into_iter()
            .map(|m| {
                let mv = player_move(&state.board, m);
                let offer = LegalMove::from_to(mv, u32::from(mv.from), u32::from(mv.to));
                match m.promotion {
                    Some(piece) => {
                        offer.with_choice(format!("game.chess.promote.{}", piece_word(piece)))
                    }
                    None => offer,
                }
            })
            .collect()
    }

    fn view(state: &State, player: u32) -> GameView {
        let board = &state.board;
        let pieces = board
            .occupied()
            .into_iter()
            .map(|square| {
                let name = piece_name(
                    board.color_on(square).unwrap(),
                    board.piece_on(square).unwrap(),
                );
                AbiPiece {
                    cell: square as u32,
                    owner: state.player(board.color_on(square).unwrap()),
                    count: 1,
                    label_key: format!("game.chess.piece.{name}"),
                    image: name,
                }
            })
            .collect();

        let mut highlights = Vec::new();
        if state.last.0 != NO_SQUARE {
            for cell in [state.last.0, state.last.1] {
                highlights.push(Highlight {
                    cell: u32::from(cell),
                    kind: highlight::Kind::LastMove.into(),
                });
            }
        }
        if !board.checkers().is_empty() {
            let king = board.king(board.side_to_move());
            highlights.push(Highlight {
                cell: king as u32,
                kind: highlight::Kind::Check.into(),
            });
        }

        let in_check = !board.checkers().is_empty();
        let status = match Self::status(state).state {
            Some(status::State::ToMove(p)) if p == player && in_check => {
                text("game.chess.status.your_turn_check")
            }
            Some(status::State::ToMove(p)) if p == player => text("game.chess.status.your_turn"),
            Some(status::State::ToMove(_)) => text("game.chess.status.their_turn"),
            Some(status::State::Finished(Outcome {
                result: Some(outcome::Result::Winner(w)),
                ..
            })) => text(if w == player {
                "game.chess.status.won"
            } else {
                "game.chess.status.lost"
            }),
            _ => text("game.chess.status.draw"),
        };

        GameView {
            board: Some(AbiBoard {
                kind: Some(board::Kind::Grid(Grid {
                    width: 8,
                    height: 8,
                })),
            }),
            pieces,
            highlights,
            status: Some(status),
            flipped: state.color(player) == Color::Black,
        }
    }

    fn status(state: &State) -> Status {
        let board = &state.board;
        let finished = |result, key: &str| {
            status::State::Finished(Outcome {
                result: Some(result),
                reason_key: key.to_string(),
            })
        };
        let draw = |key| finished(outcome::Result::Draw(Draw {}), key);
        let state = match board.status() {
            GameStatus::Won => {
                let winner = state.player(!board.side_to_move());
                finished(outcome::Result::Winner(winner), "game.chess.end.checkmate")
            }
            GameStatus::Drawn if board.halfmove_clock() < 100 => draw("game.chess.end.stalemate"),
            _ if state.repetitions() >= 3 => draw("game.chess.end.repetition"),
            _ if board.halfmove_clock() >= 100 => draw("game.chess.end.fifty_moves"),
            _ if insufficient_material(board) => draw("game.chess.end.insufficient_material"),
            _ => status::State::ToMove(state.player(board.side_to_move())),
        };
        Status { state: Some(state) }
    }
}

#[cfg(test)]
mod tests;

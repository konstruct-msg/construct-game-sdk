//! Checks a game module the way the host runs it, and then some. The audit tool for
//! third-party games, run first on Konstruct's own in CI.
//!
//! [`check`] plays random games against itself and, at every position, holds the module to
//! what the ABI promises and the match relies on:
//!
//! - every answer decodes, and no call traps or runs out of fuel;
//! - the player `status` names has moves, the other player none; every offered move applies,
//!   and nothing that was not offered does — random bytes, the other player's move, a move
//!   by chance — each refused as `invalid`, never a trap;
//! - offered moves are distinct and can be shown: a target cell on the board or a button,
//!   and a choice key wherever two moves share their cells;
//! - a roll the game asks for applies, and a roll outside what it asked for is refused;
//! - `view` for each player decodes, has a board, a status line, and names only cells
//!   that exist;
//! - a game replayed from the same seed and moves passes through the same states;
//! - the most fuel any call took is at most a tenth of the host's limit.
//!
//! What it cannot see: rules that are wrong but consistent. Chess with a wrong castling
//! rule passes; that is what a game's own tests (perft) and the human audit are for.

use std::collections::BTreeMap;
use std::fmt;

use construct_game_abi::{
    ApplyResult, ChanceRoll, GameView, Message, MoveList, MoveOption, Player, SEED_LEN, Status,
    apply_result, board, status,
};
use construct_games_host::{CallError, GameModule, Limits, Output};

/// Something that runs a module's exports. [`GameModule`] is the real one; tests implement
/// it over native code, to check that each defect the tool looks for is found.
pub trait Target {
    fn call(
        &self,
        export: &str,
        inputs: &[&[u8]],
        player: Option<u32>,
    ) -> Result<Output, CallError>;
}

impl Target for GameModule {
    fn call(
        &self,
        export: &str,
        inputs: &[&[u8]],
        player: Option<u32>,
    ) -> Result<Output, CallError> {
        GameModule::call(self, export, inputs, player)
    }
}

/// The fuel a call may take is at most this fraction of the host's limit: room for
/// positions random games did not reach.
pub const FUEL_MARGIN: u64 = 10;

const CHANCE: u32 = Player::Chance as u32;

#[derive(Debug, Clone)]
pub struct Config {
    pub games: u32,
    /// Of those, how many check every offered move at every position, not only the one
    /// played. Costly for a game with many moves, so not all.
    pub full_games: u32,
    pub max_plies: u32,
    pub seed: u64,
    pub options: Vec<u8>,
    /// Random byte strings tried as moves at each position.
    pub junk_per_ply: u32,
    pub fuel_limit: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            games: 200,
            full_games: 10,
            max_plies: 400,
            seed: 0x5EED_0FC0_FFEE,
            options: Vec::new(),
            junk_per_ply: 3,
            fuel_limit: Limits::default().fuel_per_call,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// `init` refused the options; nothing else can be checked.
    InitRefused {
        reason_key: String,
    },
    Call {
        export: &'static str,
        error: CallError,
    },
    Malformed {
        what: &'static str,
    },
    NoMovesForPlayerToMove,
    MovesForPlayerNotToMove {
        player: u32,
    },
    OfferedMoveRefused {
        mv: Vec<u8>,
        reason_key: String,
    },
    UnofferedMoveAccepted {
        mv: Vec<u8>,
    },
    OtherPlayerAccepted,
    ChanceMoveAccepted,
    RollRefused {
        values: Vec<u32>,
        reason_key: String,
    },
    BadRollAccepted {
        values: Vec<u32>,
    },
    DuplicateMove {
        mv: Vec<u8>,
    },
    /// Neither a cell to tap nor a button.
    MoveWithoutTarget {
        mv: Vec<u8>,
    },
    /// Moves sharing from/to that the app could not tell apart.
    AmbiguousChoice {
        from: Option<u32>,
        to: Option<u32>,
    },
    CellOffBoard {
        what: &'static str,
        cell: u32,
        cells: u32,
    },
    NoBoard,
    NoStatusText,
    Nondeterministic,
    FuelMargin {
        export: &'static str,
        most: u64,
        limit: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub game: u32,
    pub ply: u32,
    pub kind: Kind,
}

#[derive(Debug, Default)]
pub struct Report {
    pub findings: Vec<Finding>,
    pub games: u32,
    pub finished: u32,
    /// Games stopped at `max_plies` without an end.
    pub capped: u32,
    pub positions: u64,
    pub most_fuel: BTreeMap<&'static str, u64>,
    pub largest_state: usize,
}

impl Report {
    pub fn passed(&self) -> bool {
        self.findings.is_empty()
    }

    pub fn has(&self, matches: impl Fn(&Kind) -> bool) -> bool {
        self.findings.iter().any(|f| matches(&f.kind))
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "{} games ({} ended, {} stopped at the ply limit), {} positions, largest state {} bytes",
            self.games, self.finished, self.capped, self.positions, self.largest_state,
        )?;
        for (export, fuel) in &self.most_fuel {
            writeln!(f, "  most fuel {export:<15} {fuel:>12}")?;
        }
        if self.findings.is_empty() {
            writeln!(f, "no findings")
        } else {
            for finding in &self.findings {
                writeln!(
                    f,
                    "FINDING game {} ply {}: {:?}",
                    finding.game, finding.ply, finding.kind
                )?;
            }
            Ok(())
        }
    }
}

/// At most this many findings are kept; a module that fails everywhere fails the same way.
const MAX_FINDINGS: usize = 50;

/// A game stops at its first finding: what follows is built on a position already wrong.
struct Stop;

struct Checker<'a, T> {
    target: &'a T,
    config: &'a Config,
    rng: Rng,
    report: Report,
    game: u32,
    ply: u32,
}

pub fn check(target: &impl Target, config: &Config) -> Report {
    let mut checker = Checker {
        target,
        config,
        rng: Rng(config.seed | 1),
        report: Report::default(),
        game: 0,
        ply: 0,
    };
    for game in 0..config.games {
        checker.game = game;
        checker.ply = 0;
        checker.report.games += 1;
        let full = game < config.full_games;
        match checker.play_game(full) {
            Ok(Some((seed, moves, states))) => {
                if checker.replay(&seed, &moves, &states).is_err() {
                    continue;
                }
            }
            Ok(None) => {}
            Err(Stop) => {}
        }
        if checker.report.findings.len() >= MAX_FINDINGS
            || checker
                .report
                .has(|k| matches!(k, Kind::InitRefused { .. }))
        {
            break;
        }
    }
    let limit = config.fuel_limit;
    for (&export, &most) in &checker.report.most_fuel {
        if most.saturating_mul(FUEL_MARGIN) > limit {
            checker.report.findings.push(Finding {
                game: 0,
                ply: 0,
                kind: Kind::FuelMargin {
                    export,
                    most,
                    limit,
                },
            });
        }
    }
    checker.report
}

type Played = Option<([u8; SEED_LEN], Vec<(u32, Vec<u8>)>, Vec<Vec<u8>>)>;

impl<T: Target> Checker<'_, T> {
    fn find(&mut self, kind: Kind) -> Stop {
        if self.report.findings.len() < MAX_FINDINGS {
            self.report.findings.push(Finding {
                game: self.game,
                ply: self.ply,
                kind,
            });
        }
        Stop
    }

    fn call(
        &mut self,
        export: &'static str,
        inputs: &[&[u8]],
        player: Option<u32>,
    ) -> Result<Vec<u8>, Stop> {
        match self.target.call(export, inputs, player) {
            Ok(output) => {
                let most = self.report.most_fuel.entry(export).or_default();
                *most = (*most).max(output.fuel);
                Ok(output.bytes)
            }
            Err(error) => Err(self.find(Kind::Call { export, error })),
        }
    }

    /// `Ok(state)` or `Err(reason)` for a refusal; a malformed answer is a finding.
    fn apply(
        &mut self,
        state: &[u8],
        mv: &[u8],
        player: u32,
    ) -> Result<Result<Vec<u8>, String>, Stop> {
        let bytes = self.call("cg_apply", &[state, mv], Some(player))?;
        self.applied(&bytes)
    }

    fn applied(&mut self, bytes: &[u8]) -> Result<Result<Vec<u8>, String>, Stop> {
        match ApplyResult::decode(bytes).ok().and_then(|r| r.result) {
            Some(apply_result::Result::State(state)) => Ok(Ok(state)),
            Some(apply_result::Result::Invalid(invalid)) => Ok(Err(invalid.reason_key)),
            None => Err(self.find(Kind::Malformed {
                what: "ApplyResult",
            })),
        }
    }

    fn status(&mut self, state: &[u8]) -> Result<status::State, Stop> {
        let bytes = self.call("cg_status", &[state], None)?;
        match Status::decode(bytes.as_slice()).ok().and_then(|s| s.state) {
            Some(state) => Ok(state),
            None => Err(self.find(Kind::Malformed { what: "Status" })),
        }
    }

    fn legal_moves(&mut self, state: &[u8], player: u32) -> Result<Vec<MoveOption>, Stop> {
        let bytes = self.call("cg_legal_moves", &[state], Some(player))?;
        match MoveList::decode(bytes.as_slice()) {
            Ok(list) => Ok(list.moves),
            Err(_) => Err(self.find(Kind::Malformed { what: "MoveList" })),
        }
    }

    /// Checks both players' views; returns the number of cells on the board.
    fn views(&mut self, state: &[u8]) -> Result<u32, Stop> {
        let mut cells = 0;
        for player in [0, 1] {
            let bytes = self.call("cg_view", &[state], Some(player))?;
            let Ok(view) = GameView::decode(bytes.as_slice()) else {
                return Err(self.find(Kind::Malformed { what: "GameView" }));
            };
            cells = match view.board.and_then(|b| b.kind) {
                Some(board::Kind::Grid(g)) => g.width * g.height,
                Some(board::Kind::Intersections(i)) => i.size * i.size,
                Some(board::Kind::Points24(_)) => 28,
                None => return Err(self.find(Kind::NoBoard)),
            };
            if view.status.is_none_or(|t| t.key.is_empty()) {
                return Err(self.find(Kind::NoStatusText));
            }
            let named = view
                .pieces
                .iter()
                .map(|p| ("piece", p.cell))
                .chain(view.highlights.iter().map(|h| ("highlight", h.cell)));
            for (what, cell) in named {
                if cell >= cells {
                    return Err(self.find(Kind::CellOffBoard { what, cell, cells }));
                }
            }
        }
        Ok(cells)
    }

    fn check_offers(&mut self, moves: &[MoveOption], cells: u32) -> Result<(), Stop> {
        let mut seen = BTreeMap::new();
        let mut by_cells: BTreeMap<(Option<u32>, Option<u32>), Vec<&str>> = BTreeMap::new();
        for m in moves {
            if seen.insert(m.r#move.clone(), ()).is_some() {
                return Err(self.find(Kind::DuplicateMove {
                    mv: m.r#move.clone(),
                }));
            }
            if m.from.is_none() && m.to.is_none() && m.action_key.is_empty() {
                return Err(self.find(Kind::MoveWithoutTarget {
                    mv: m.r#move.clone(),
                }));
            }
            for cell in m.from.into_iter().chain(m.to) {
                if cell >= cells {
                    return Err(self.find(Kind::CellOffBoard {
                        what: "move",
                        cell,
                        cells,
                    }));
                }
            }
            if m.from.is_some() || m.to.is_some() {
                by_cells
                    .entry((m.from, m.to))
                    .or_default()
                    .push(&m.choice_key);
            }
        }
        for ((from, to), mut keys) in by_cells {
            if keys.len() > 1 {
                keys.sort();
                let distinct = keys.windows(2).all(|w| w[0] != w[1]);
                if !distinct || keys.iter().any(|k| k.is_empty()) {
                    return Err(self.find(Kind::AmbiguousChoice { from, to }));
                }
            }
        }
        Ok(())
    }

    fn play_game(&mut self, full: bool) -> Result<Played, Stop> {
        let seed: [u8; SEED_LEN] = self.rng.bytes();
        let options = self.config.options.clone();
        let init = self.call("cg_init", &[&seed, &options], None)?;
        let mut state = match self.applied(&init)? {
            Ok(state) => state,
            Err(reason_key) => return Err(self.find(Kind::InitRefused { reason_key })),
        };
        let mut moves = Vec::new();
        let mut states = vec![state.clone()];

        for ply in 0..self.config.max_plies {
            self.ply = ply;
            self.report.positions += 1;
            self.report.largest_state = self.report.largest_state.max(state.len());
            let cells = self.views(&state)?;
            let (player, mv) = match self.status(&state)? {
                status::State::Finished(_) => {
                    self.report.finished += 1;
                    return Ok(Some((seed, moves, states)));
                }
                status::State::AwaitingChance(spec) => {
                    for player in [0, 1] {
                        if !self.legal_moves(&state, player)?.is_empty() {
                            return Err(self.find(Kind::MovesForPlayerNotToMove { player }));
                        }
                    }
                    self.check_bad_rolls(&state, &spec.dice)?;
                    let values: Vec<u32> = spec
                        .dice
                        .iter()
                        .map(|&s| 1 + self.rng.below(s.max(1) as usize) as u32)
                        .collect();
                    let roll = ChanceRoll {
                        values: values.clone(),
                    }
                    .encode_to_vec();
                    if let Err(reason_key) = self.apply(&state, &roll, CHANCE)? {
                        return Err(self.find(Kind::RollRefused { values, reason_key }));
                    }
                    (CHANCE, roll)
                }
                status::State::ToMove(player) => {
                    let mv = self.check_position(&state, player, cells, full)?;
                    (player, mv)
                }
            };
            let next = match self.apply(&state, &mv, player)? {
                Ok(next) => next,
                Err(reason_key) => {
                    return Err(self.find(Kind::OfferedMoveRefused { mv, reason_key }));
                }
            };
            moves.push((player, mv));
            states.push(next.clone());
            state = next;
        }
        self.report.capped += 1;
        Ok(Some((seed, moves, states)))
    }

    /// Everything about a position where `player` is to move; returns the move to play.
    fn check_position(
        &mut self,
        state: &[u8],
        player: u32,
        cells: u32,
        full: bool,
    ) -> Result<Vec<u8>, Stop> {
        let other = 1 - player;
        if !self.legal_moves(state, other)?.is_empty() {
            return Err(self.find(Kind::MovesForPlayerNotToMove { player: other }));
        }
        let offers = self.legal_moves(state, player)?;
        if offers.is_empty() {
            return Err(self.find(Kind::NoMovesForPlayerToMove));
        }
        self.check_offers(&offers, cells)?;

        let first = offers[0].r#move.clone();
        if self.apply(state, &first, other)?.is_ok() {
            return Err(self.find(Kind::OtherPlayerAccepted));
        }
        if self.apply(state, &first, CHANCE)?.is_ok() {
            return Err(self.find(Kind::ChanceMoveAccepted));
        }
        for _ in 0..self.config.junk_per_ply {
            let len = self.rng.below(9);
            let junk: Vec<u8> = (0..len).map(|_| self.rng.next() as u8).collect();
            if offers.iter().any(|m| m.r#move == junk) {
                continue;
            }
            if self.apply(state, &junk, player)?.is_ok() {
                return Err(self.find(Kind::UnofferedMoveAccepted { mv: junk }));
            }
        }
        if full {
            for offer in &offers {
                if let Err(reason_key) = self.apply(state, &offer.r#move, player)? {
                    return Err(self.find(Kind::OfferedMoveRefused {
                        mv: offer.r#move.clone(),
                        reason_key,
                    }));
                }
            }
        }
        Ok(offers[self.rng.below(offers.len())].r#move.clone())
    }

    fn check_bad_rolls(&mut self, state: &[u8], dice: &[u32]) -> Result<(), Stop> {
        let mut bad = vec![vec![0; dice.len()], vec![1; dice.len() + 1]];
        if let Some(&sides) = dice.first() {
            let mut over = vec![1; dice.len()];
            over[0] = sides + 1;
            bad.push(over);
        }
        for values in bad {
            let roll = ChanceRoll {
                values: values.clone(),
            }
            .encode_to_vec();
            if self.apply(state, &roll, CHANCE)?.is_ok() {
                return Err(self.find(Kind::BadRollAccepted { values }));
            }
        }
        Ok(())
    }

    /// The same seed and moves again: every state must come out byte for byte the same.
    fn replay(
        &mut self,
        seed: &[u8; SEED_LEN],
        moves: &[(u32, Vec<u8>)],
        states: &[Vec<u8>],
    ) -> Result<(), Stop> {
        let options = self.config.options.clone();
        let init = self.call("cg_init", &[seed, &options], None)?;
        let mut state = match self.applied(&init)? {
            Ok(state) => state,
            Err(_) => return Err(self.find(Kind::Nondeterministic)),
        };
        for (ply, ((player, mv), expected)) in moves.iter().zip(states).enumerate() {
            self.ply = ply as u32;
            if state != *expected {
                return Err(self.find(Kind::Nondeterministic));
            }
            state = match self.apply(&state, mv, *player)? {
                Ok(next) => next,
                Err(_) => return Err(self.find(Kind::Nondeterministic)),
            };
        }
        if Some(&state) != states.last() {
            return Err(self.find(Kind::Nondeterministic));
        }
        Ok(())
    }
}

/// xorshift64*: the same seed checks the same games, so a finding can be reproduced.
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

    fn bytes<const N: usize>(&mut self) -> [u8; N] {
        let mut out = [0u8; N];
        out.iter_mut().for_each(|b| *b = self.next() as u8);
        out
    }
}

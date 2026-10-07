//! Shared by the match tests: `Rules` over a game's native code, and a dice game.

#![allow(dead_code)]

use std::marker::PhantomData;

use construct_game_abi::{ChanceSpec, Draw, GameView, Message, MoveList, SEED_LEN, Status};
use construct_game_abi::{outcome, status};
use construct_game_sdk::{
    ChanceRoll, Codec, Game, Invalid, LegalMove, Outcome, PLAYER_0, PLAYER_1, bytes,
};
use construct_games_host::{Applied, CallError, GameId, Rules};
use sha2::{Digest, Sha256};

/// A game's native code behind `Rules`, through the same byte-level functions its module
/// runs — so the match sees exactly what it would see through the host, minus the cost
/// of an instance per call.
pub struct Native<G> {
    id: GameId,
    game: PhantomData<G>,
}

impl<G: Game> Native<G> {
    pub fn new(name: &str) -> Self {
        Self {
            id: GameId(Sha256::digest(name.as_bytes()).into()),
            game: PhantomData,
        }
    }
}

impl<G: Game> Rules for Native<G> {
    fn id(&self) -> GameId {
        self.id
    }

    fn init(&self, seed: &[u8; SEED_LEN], options: &[u8]) -> Result<Applied, CallError> {
        Applied::decode(&bytes::init::<G>(seed, options).ok_or(CallError::ModuleFailed)?)
    }

    fn apply(&self, state: &[u8], mv: &[u8], player: u32) -> Result<Applied, CallError> {
        Applied::decode(&bytes::apply::<G>(state, mv, player).ok_or(CallError::ModuleFailed)?)
    }

    fn legal_moves(&self, state: &[u8], player: u32) -> Result<MoveList, CallError> {
        let bytes = bytes::legal_moves::<G>(state, player).ok_or(CallError::ModuleFailed)?;
        MoveList::decode(bytes.as_slice()).map_err(|_| CallError::Malformed("MoveList"))
    }

    fn status(&self, state: &[u8]) -> Result<Status, CallError> {
        let bytes = bytes::status::<G>(state).ok_or(CallError::ModuleFailed)?;
        Status::decode(bytes.as_slice()).map_err(|_| CallError::Malformed("Status"))
    }
}

/// A race to `GOAL` on one die. A turn starts with a roll: even, the player who rolled
/// moves; odd, the move passes to the other player. The mover advances by the roll or by
/// one. It opens on a roll, and after a roll either player may be to move — the two
/// cases the match's reveal timing has to get right.
pub struct DiceRace;

pub const GOAL: u8 = 30;

/// `[position 0, position 1, to move, roll]`; roll 0 means the next thing is a roll.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RaceState([u8; 4]);

impl Codec for RaceState {
    fn encode(&self) -> Vec<u8> {
        self.0.to_vec()
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        let s: [u8; 4] = bytes.try_into().ok()?;
        (s[0] <= GOAL + 6 && s[1] <= GOAL + 6 && s[2] <= 1 && s[3] <= 6).then_some(Self(s))
    }
}

/// 0: advance by the roll; 1: advance by one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RaceMove(u8);

impl Codec for RaceMove {
    fn encode(&self) -> Vec<u8> {
        vec![self.0]
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        match bytes {
            [m] if *m <= 1 => Some(Self(*m)),
            _ => None,
        }
    }
}

impl Game for DiceRace {
    type State = RaceState;
    type Move = RaceMove;
    type Options = ();

    fn init(seed: &[u8; SEED_LEN], _: ()) -> Result<RaceState, Invalid> {
        Ok(RaceState([0, 0, seed[0] & 1, 0]))
    }

    fn apply(state: &RaceState, mv: &RaceMove, player: u32) -> Result<RaceState, Invalid> {
        let [mut p0, mut p1, _, roll] = state.0;
        let step = if mv.0 == 0 { roll } else { 1 };
        if player == PLAYER_0 {
            p0 += step
        } else {
            p1 += step
        }
        Ok(RaceState([p0, p1, 1 - player as u8, 0]))
    }

    fn apply_chance(state: &RaceState, roll: &ChanceRoll) -> Result<RaceState, Invalid> {
        let [p0, p1, to_move, _] = state.0;
        let value = roll.values[0] as u8;
        let to_move = if value.is_multiple_of(2) {
            to_move
        } else {
            1 - to_move
        };
        Ok(RaceState([p0, p1, to_move, value]))
    }

    fn legal_moves(state: &RaceState, player: u32) -> Vec<LegalMove<RaceMove>> {
        match Self::status(state).state {
            Some(status::State::ToMove(p)) if p == player => vec![
                LegalMove::action(RaceMove(0), "game.dicerace.by_roll"),
                LegalMove::action(RaceMove(1), "game.dicerace.by_one"),
            ],
            _ => Vec::new(),
        }
    }

    fn view(_: &RaceState, player: u32) -> GameView {
        GameView {
            orientation: player,
            ..GameView::default()
        }
    }

    fn status(state: &RaceState) -> Status {
        let [p0, p1, to_move, roll] = state.0;
        let finished = |winner| {
            status::State::Finished(Outcome {
                result: Some(winner),
                reason_key: "game.dicerace.end".into(),
            })
        };
        let state = match (p0 >= GOAL, p1 >= GOAL) {
            (true, true) => finished(outcome::Result::Draw(Draw {})),
            (true, false) => finished(outcome::Result::Winner(PLAYER_0)),
            (false, true) => finished(outcome::Result::Winner(PLAYER_1)),
            _ if roll == 0 => status::State::AwaitingChance(ChanceSpec { dice: vec![6] }),
            _ => status::State::ToMove(u32::from(to_move)),
        };
        Status { state: Some(state) }
    }
}

/// xorshift64*: deterministic, so a failing game can be replayed by its number.
pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    pub fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }

    pub fn bytes<const N: usize>(&mut self) -> [u8; N] {
        let mut out = [0u8; N];
        out.iter_mut().for_each(|b| *b = self.next() as u8);
        out
    }
}

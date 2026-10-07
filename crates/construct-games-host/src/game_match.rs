//! A match between two players: the protocol both sides run, as a pure state machine.
//!
//! A [`Match`] takes events — the local player's actions, the other side's messages — and
//! returns the messages to send. It owns no clock, no network and no randomness: the
//! caller supplies the match id and a 32-byte secret, delivers messages however it likes,
//! and may lose, repeat or reorder them.
//!
//! - **Every message is numbered** by its sender (`n` = 1, 2, …) and processed in that
//!   order. A repeat with the same content is dropped; a repeat with different content is
//!   a [`Divergence`]; a message from the future waits for the gap. [`Match::sent`] is
//!   everything this side ever sent, so the caller can resend it after a loss.
//! - **After every move both sides hash the state** and compare: the mover sends its hash
//!   with the move. A mismatch stops the match as [`Divergence::StateHash`]; nothing is
//!   repaired silently.
//! - **Turn order is checked here**, against the game's own `status`, not left to the
//!   module: a third-party module need not use the SDK that checks it.
//! - **Randomness is a hash chain per side.** Each side keeps a secret `s_N` and commits to
//!   `s_0 = H^N(s_N)` in its first message. The value for index `k` is `s_k`, checked by
//!   `H(s_k) = s_{k-1}`, so a side cannot choose it after the commit and cannot work out
//!   the other side's before it is revealed. Index 1 seeds the game; each roll the game
//!   asks for takes the next index. A side reveals `s_k` only once the game is waiting
//!   for roll `k` — never earlier, which would let the other side see a future roll — and
//!   carries it on the next message it sends.
//! - **Resigning and draws are the match's**, the same for every game. While a side's
//!   draw offer stands it can neither move nor resign, and a resignation at ply `p`
//!   outranks a game end after `p`: with these rules every crossing of two actions in
//!   flight ends the same on both sides (`tests/two_sides.rs` crosses them at random).
//! - **What a side can still do is stop.** The inviter learns the seed first, and the
//!   side that moves after a roll sees the roll first; either may then never send
//!   another message. The match cannot tell that from someone who left, and has no clock
//!   to judge it: it shows the other side's turn, without a deadline.
//!
//! Agreement assumes every message is eventually delivered: the caller resends
//! [`Match::sent`] until the other side has it.
//!
//! Messages are Rust values here; their wire form is plan stage 7 (`construct-protos`).

use std::collections::BTreeMap;

use construct_game_abi::{ChanceRoll, Invalid, Message as _, SEED_LEN, outcome, status};
use sha2::{Digest, Sha256};

use crate::{Applied, CallError, GameId, Rules};

pub type Hash = [u8; 32];

/// Values in each side's chain; one is spent on the seed and one per roll. A backgammon
/// game takes about a hundred.
pub const CHAIN_LEN: u32 = 1024;

pub const INVITER: u32 = 0;
pub const INVITEE: u32 = 1;

const PLAYER_CHANCE: u32 = construct_game_abi::Player::Chance as u32;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MatchId(pub [u8; 16]);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    pub match_id: MatchId,
    /// The sender's count of messages in this match, from 1.
    pub n: u32,
    /// The sender's chain values, continuing from the last one it revealed.
    pub reveals: Vec<Hash>,
    pub body: Body,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Body {
    Invite {
        game: GameId,
        options: Vec<u8>,
        commit: Hash,
    },
    /// Carries the invitee's value 1.
    Accept {
        commit: Hash,
    },
    Decline,
    /// The inviter's value 1: with it the invitee can seed the game too.
    Begin,
    /// `ply` is the number of moves made before this one.
    Move {
        ply: u32,
        mv: Vec<u8>,
        state_hash: Hash,
    },
    /// Only values; sent when a roll needs them and there is no move to carry them.
    Reveal,
    Resign {
        ply: u32,
    },
    DrawOffer {
        ply: u32,
    },
    DrawAnswer {
        ply: u32,
        accept: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outcome {
    /// `None` for a draw.
    pub winner: Option<u32>,
    pub reason: Reason,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reason {
    /// The game's own end, with its reason key.
    Game {
        reason_key: String,
    },
    Resignation,
    /// Both resigned at the same position.
    BothResigned,
    DrawAgreed,
}

/// Why a match stopped. Terminal: a diverged match is shown as such and not repaired.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Divergence {
    StateHash {
        ply: u32,
        ours: Hash,
        theirs: Hash,
    },
    /// The other side sent a move the game refuses.
    IllegalMove {
        ply: u32,
        invalid: Invalid,
    },
    Ply {
        expected: u32,
        got: u32,
    },
    NotTheirTurn {
        ply: u32,
    },
    /// A chain value that does not hash to the one before it.
    BadReveal {
        index: u32,
    },
    /// The game started without the value that seeds it.
    MissingReveal {
        index: u32,
    },
    ChainExhausted,
    /// The same message number with different content.
    Conflict {
        n: u32,
    },
    /// A message this phase does not expect.
    Unexpected {
        n: u32,
    },
    /// The game refused a roll the host computed from both chains.
    RollRefused(Invalid),
    /// The module failed on a state both sides should share.
    Module(CallError),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Invite sent, no answer yet.
    Inviting,
    /// Invite received, not answered.
    Invited,
    /// Accepted; the inviter's `Begin` has not arrived.
    AwaitingBegin,
    Declined,
    Playing,
    Finished(Outcome),
    Diverged(Divergence),
}

/// Why a local action was refused. Nothing is sent and nothing changes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlayError {
    NotPlaying,
    NotYourTurn,
    /// The rules given are not the game this match plays.
    WrongGame,
    /// This side's draw offer is waiting for an answer; see [`Match::offer_draw`].
    OfferPending,
    NoOfferToAnswer,
    Invalid(Invalid),
    Module(CallError),
}

pub struct Match {
    id: MatchId,
    game: GameId,
    options: Vec<u8>,
    side: u32,
    /// Our chain, `chain[k]` = value `k`; `chain[CHAIN_LEN]` is the secret. Computed once:
    /// deriving a value from the secret each time costs up to `CHAIN_LEN` hashes.
    chain: Vec<Hash>,
    their_commit: Option<Hash>,
    /// Their verified chain values; index `k` is at `k - 1`.
    their_values: Vec<Hash>,
    /// The highest index of ours the game has needed, and how many we have sent.
    needed: u32,
    revealed: u32,
    sent: Vec<Message>,
    received: Vec<Message>,
    waiting: BTreeMap<u32, Message>,
    phase: Phase,
    state: Vec<u8>,
    ply: u32,
    /// The last chain index spent: 1 once seeded, then one per roll.
    spent: u32,
    /// `(who offered, at which ply)`.
    draw_offer: Option<(u32, u32)>,
    /// The ply each side resigned at.
    resigned: [Option<u32>; 2],
    /// The ply at which the game itself ended.
    ended_at: Option<u32>,
}

impl Match {
    /// Starts a match as the inviter. `secret` must be fresh random bytes, never reused.
    pub fn invite(
        rules: &impl Rules,
        id: MatchId,
        options: Vec<u8>,
        secret: Hash,
    ) -> (Self, Message) {
        let mut this = Self::new(id, rules.id(), options, INVITER, secret, Phase::Inviting);
        let body = Body::Invite {
            game: this.game,
            options: this.options.clone(),
            commit: this.value(0),
        };
        let message = this.send(body);
        (this, message)
    }

    /// The invitee's side of a match, from the invitation. Returns `None` if `message` is
    /// not a first message inviting to a match.
    pub fn invited(message: &Message, secret: Hash) -> Option<Self> {
        let Body::Invite {
            game,
            options,
            commit,
        } = &message.body
        else {
            return None;
        };
        if message.n != 1 || !message.reveals.is_empty() {
            return None;
        }
        let mut this = Self::new(
            message.match_id,
            *game,
            options.clone(),
            INVITEE,
            secret,
            Phase::Invited,
        );
        this.their_commit = Some(*commit);
        this.received.push(message.clone());
        Some(this)
    }

    fn new(
        id: MatchId,
        game: GameId,
        options: Vec<u8>,
        side: u32,
        secret: Hash,
        phase: Phase,
    ) -> Self {
        Self {
            id,
            game,
            options,
            side,
            chain: chain(secret),
            their_commit: None,
            their_values: Vec::new(),
            needed: 0,
            revealed: 0,
            sent: Vec::new(),
            received: Vec::new(),
            waiting: BTreeMap::new(),
            phase,
            state: Vec::new(),
            ply: 0,
            spent: 0,
            draw_offer: None,
            resigned: [None, None],
            ended_at: None,
        }
    }

    pub fn id(&self) -> MatchId {
        self.id
    }

    pub fn game(&self) -> GameId {
        self.game
    }

    pub fn options(&self) -> &[u8] {
        &self.options
    }

    pub fn side(&self) -> u32 {
        self.side
    }

    pub fn phase(&self) -> &Phase {
        &self.phase
    }

    /// The game's state; empty until the game has been seeded.
    pub fn state(&self) -> &[u8] {
        &self.state
    }

    pub fn ply(&self) -> u32 {
        self.ply
    }

    /// Every message this side has sent, in order — what to resend after a loss.
    pub fn sent(&self) -> &[Message] {
        &self.sent
    }

    /// The last chain index spent: 1 once the game is seeded, then one per roll.
    pub fn rolls_spent(&self) -> u32 {
        self.spent
    }

    /// `(who offered, at which ply)` while a draw offer stands.
    pub fn draw_offer(&self) -> Option<(u32, u32)> {
        self.draw_offer
    }

    fn other(&self) -> u32 {
        1 - self.side
    }

    // ── local actions ───────────────────────────────────────────────────────────

    pub fn accept(&mut self, rules: &impl Rules) -> Result<Vec<Message>, PlayError> {
        if self.phase != Phase::Invited {
            return Err(PlayError::NotPlaying);
        }
        if rules.id() != self.game {
            return Err(PlayError::WrongGame);
        }
        self.needed = 1;
        self.phase = Phase::AwaitingBegin;
        let commit = self.value(0);
        Ok(vec![self.send(Body::Accept { commit })])
    }

    pub fn decline(&mut self) -> Result<Vec<Message>, PlayError> {
        if self.phase != Phase::Invited {
            return Err(PlayError::NotPlaying);
        }
        self.phase = Phase::Declined;
        Ok(vec![self.send(Body::Decline)])
    }

    /// Makes this side's move.
    pub fn play(&mut self, rules: &impl Rules, mv: &[u8]) -> Result<Vec<Message>, PlayError> {
        self.check_can_act(rules)?;
        match self.status(rules).map_err(PlayError::Module)? {
            status::State::ToMove(p) if p == self.side => {}
            _ => return Err(PlayError::NotYourTurn),
        }
        let next = match rules
            .apply(&self.state, mv, self.side)
            .map_err(PlayError::Module)?
        {
            Applied::State(next) => next,
            Applied::Invalid(invalid) => return Err(PlayError::Invalid(invalid)),
        };
        let body = Body::Move {
            ply: self.ply,
            mv: mv.to_vec(),
            state_hash: state_hash(&next),
        };
        self.state = next;
        self.ply += 1;
        // A move answers a standing offer from the other side with no.
        self.draw_offer = None;
        // Settled before sending, so the move carries the value a roll it caused needs.
        self.settle(rules);
        let mut out = vec![self.send(body)];
        out.extend(self.flush(rules));
        Ok(out)
    }

    pub fn resign(&mut self, rules: &impl Rules) -> Result<Vec<Message>, PlayError> {
        self.check_can_act(rules)?;
        self.resigned[self.side as usize] = Some(self.ply);
        self.conclude_resignations();
        Ok(vec![self.send(Body::Resign { ply: self.ply })])
    }

    /// Offers a draw at the current position, or — if the other side's offer for this
    /// position stands — accepts it.
    ///
    /// While this side's offer stands, it can neither move nor resign. Without that
    /// rule the two sides could disagree: an acceptance crossing a move or a resignation
    /// would end the match differently on each side. The other side answers, or moves,
    /// which declines.
    pub fn offer_draw(&mut self, rules: &impl Rules) -> Result<Vec<Message>, PlayError> {
        self.check_can_act(rules)?;
        match self.draw_offer {
            Some((who, ply)) if who == self.other() && ply == self.ply => {
                self.answer_draw(rules, true)
            }
            _ => {
                self.draw_offer = Some((self.side, self.ply));
                Ok(vec![self.send(Body::DrawOffer { ply: self.ply })])
            }
        }
    }

    pub fn answer_draw(
        &mut self,
        rules: &impl Rules,
        accept: bool,
    ) -> Result<Vec<Message>, PlayError> {
        if self.phase != Phase::Playing {
            return Err(PlayError::NotPlaying);
        }
        if rules.id() != self.game {
            return Err(PlayError::WrongGame);
        }
        let Some((who, ply)) = self.draw_offer else {
            return Err(PlayError::NoOfferToAnswer);
        };
        if who != self.other() {
            return Err(PlayError::NoOfferToAnswer);
        }
        self.draw_offer = None;
        if accept {
            self.finish(None, Reason::DrawAgreed);
        }
        Ok(vec![self.send(Body::DrawAnswer { ply, accept })])
    }

    fn check_can_act(&self, rules: &impl Rules) -> Result<(), PlayError> {
        if self.phase != Phase::Playing {
            return Err(PlayError::NotPlaying);
        }
        if rules.id() != self.game {
            return Err(PlayError::WrongGame);
        }
        if matches!(self.draw_offer, Some((who, _)) if who == self.side) {
            return Err(PlayError::OfferPending);
        }
        Ok(())
    }

    // ── the other side's messages ───────────────────────────────────────────────

    /// Takes one message from the other side, in any order and any number of times.
    /// Returns what to send in reply.
    pub fn receive(&mut self, rules: &impl Rules, message: Message) -> Vec<Message> {
        if message.match_id != self.id || message.n == 0 || rules.id() != self.game {
            return Vec::new();
        }
        if matches!(self.phase, Phase::Diverged(_)) {
            return Vec::new();
        }
        let n = message.n;
        if let Some(seen) = self.received.get(n as usize - 1) {
            if *seen != message {
                self.diverge(Divergence::Conflict { n });
            }
            return Vec::new();
        }
        if let Some(waiting) = self.waiting.get(&n) {
            if *waiting != message {
                self.diverge(Divergence::Conflict { n });
            }
            return Vec::new();
        }
        self.waiting.insert(n, message);

        let mut out = Vec::new();
        while let Some(next) = self.waiting.remove(&(self.received.len() as u32 + 1)) {
            self.received.push(next.clone());
            out.extend(self.process(rules, next));
            if matches!(self.phase, Phase::Diverged(_)) {
                break;
            }
        }
        out
    }

    fn process(&mut self, rules: &impl Rules, message: Message) -> Vec<Message> {
        let n = message.n;
        if let Body::Accept { commit } = message.body {
            if self.phase != Phase::Inviting {
                self.diverge(Divergence::Unexpected { n });
                return Vec::new();
            }
            self.their_commit = Some(commit);
        }
        for value in &message.reveals {
            if let Err(divergence) = self.take_value(*value) {
                self.diverge(divergence);
                return Vec::new();
            }
        }

        match (&self.phase, message.body) {
            (Phase::Inviting, Body::Accept { .. }) => {
                self.needed = 1;
                self.start(rules);
                if self.phase == Phase::Playing || matches!(self.phase, Phase::Finished(_)) {
                    // Begin carries our value 1 even when the game opened on our move.
                    let mut out = vec![self.send(Body::Begin)];
                    out.extend(self.flush(rules));
                    return out;
                }
                Vec::new()
            }
            (Phase::Inviting, Body::Decline) => {
                self.phase = Phase::Declined;
                Vec::new()
            }
            (Phase::AwaitingBegin, Body::Begin) => {
                self.start(rules);
                self.flush(rules)
            }
            (Phase::Playing, body) => {
                self.settle(rules);
                if self.phase == Phase::Playing {
                    self.play_theirs(rules, n, body);
                }
                self.flush(rules)
            }
            (Phase::Finished(_), Body::Resign { ply }) => {
                self.resigned[self.other() as usize] = Some(ply);
                self.conclude_resignations();
                Vec::new()
            }
            // Anything else that crosses the end of the game changes nothing.
            (Phase::Finished(_), _) => Vec::new(),
            _ => {
                self.diverge(Divergence::Unexpected { n });
                Vec::new()
            }
        }
    }

    fn play_theirs(&mut self, rules: &impl Rules, n: u32, body: Body) {
        let them = self.other();
        match body {
            Body::Move {
                ply,
                mv,
                state_hash: theirs,
            } => {
                if ply != self.ply {
                    return self.diverge(Divergence::Ply {
                        expected: self.ply,
                        got: ply,
                    });
                }
                match self.status(rules) {
                    Ok(status::State::ToMove(p)) if p == them => {}
                    Ok(_) => return self.diverge(Divergence::NotTheirTurn { ply }),
                    Err(e) => return self.diverge(Divergence::Module(e)),
                }
                let next = match rules.apply(&self.state, &mv, them) {
                    Ok(Applied::State(next)) => next,
                    Ok(Applied::Invalid(invalid)) => {
                        return self.diverge(Divergence::IllegalMove { ply, invalid });
                    }
                    Err(e) => return self.diverge(Divergence::Module(e)),
                };
                let ours = state_hash(&next);
                if ours != theirs {
                    return self.diverge(Divergence::StateHash { ply, ours, theirs });
                }
                self.state = next;
                self.ply += 1;
                self.draw_offer = None;
                self.settle(rules);
            }
            Body::Reveal => self.settle(rules),
            Body::Resign { ply } => {
                self.resigned[them as usize] = Some(ply);
                self.conclude_resignations();
            }
            Body::DrawOffer { ply } => {
                if ply != self.ply {
                    // Made before a move this side has since seen: that move declined it.
                    return;
                }
                if self.draw_offer == Some((self.side, ply)) {
                    // Both offered the same position.
                    self.draw_offer = None;
                    self.finish(None, Reason::DrawAgreed);
                } else {
                    self.draw_offer = Some((them, ply));
                }
            }
            Body::DrawAnswer { ply, accept } => {
                if self.draw_offer != Some((self.side, ply)) {
                    return self.diverge(Divergence::Unexpected { n });
                }
                self.draw_offer = None;
                if accept {
                    self.finish(None, Reason::DrawAgreed);
                }
            }
            Body::Invite { .. } | Body::Accept { .. } | Body::Decline | Body::Begin => {
                self.diverge(Divergence::Unexpected { n });
            }
        }
    }

    // ── chain and game state ────────────────────────────────────────────────────

    /// Seeds and initialises the game once both values 1 are known.
    fn start(&mut self, rules: &impl Rules) {
        let Some(&theirs) = self.their_values.first() else {
            return self.diverge(Divergence::MissingReveal { index: 1 });
        };
        let ours = self.value(1);
        let (inviter, invitee) = if self.side == INVITER {
            (ours, theirs)
        } else {
            (theirs, ours)
        };
        let seed = seed(self.id, &inviter, &invitee);
        match rules.init(&seed, &self.options) {
            Ok(Applied::State(state)) => self.state = state,
            Ok(Applied::Invalid(invalid)) => return self.diverge(Divergence::RollRefused(invalid)),
            Err(e) => return self.diverge(Divergence::Module(e)),
        }
        self.spent = 1;
        self.phase = Phase::Playing;
        self.settle(rules);
    }

    /// Runs the game forward through every roll both values are known for, and notes
    /// the game's end.
    fn settle(&mut self, rules: &impl Rules) {
        while self.phase == Phase::Playing {
            match self.status(rules) {
                Err(e) => return self.diverge(Divergence::Module(e)),
                Ok(status::State::ToMove(_)) => return,
                Ok(status::State::Finished(game_outcome)) => {
                    let winner = match game_outcome.result {
                        Some(outcome::Result::Winner(w)) => Some(w),
                        _ => None,
                    };
                    self.ended_at = Some(self.ply);
                    let reason = Reason::Game {
                        reason_key: game_outcome.reason_key,
                    };
                    return self.finish(winner, reason);
                }
                Ok(status::State::AwaitingChance(spec)) => {
                    let index = self.spent + 1;
                    if index > CHAIN_LEN {
                        return self.diverge(Divergence::ChainExhausted);
                    }
                    self.needed = self.needed.max(index);
                    let Some(&theirs) = self.their_values.get(index as usize - 1) else {
                        return;
                    };
                    let ours = self.value(index);
                    let (v0, v1) = if self.side == 0 {
                        (ours, theirs)
                    } else {
                        (theirs, ours)
                    };
                    let roll = ChanceRoll {
                        values: dice(self.id, index, &v0, &v1, &spec.dice),
                    };
                    match rules.apply(&self.state, &roll.encode_to_vec(), PLAYER_CHANCE) {
                        Ok(Applied::State(next)) => {
                            self.state = next;
                            self.spent = index;
                        }
                        Ok(Applied::Invalid(invalid)) => {
                            return self.diverge(Divergence::RollRefused(invalid));
                        }
                        Err(e) => return self.diverge(Divergence::Module(e)),
                    }
                }
            }
        }
    }

    /// Sends the values the game needs from us when no move of ours will carry them: if
    /// it is our move, the move carries them.
    fn flush(&mut self, rules: &impl Rules) -> Vec<Message> {
        if self.phase != Phase::Playing || self.revealed >= self.needed {
            return Vec::new();
        }
        if matches!(self.status(rules), Ok(status::State::ToMove(p)) if p == self.side) {
            return Vec::new();
        }
        vec![self.send(Body::Reveal)]
    }

    fn take_value(&mut self, value: Hash) -> Result<(), Divergence> {
        let index = self.their_values.len() as u32 + 1;
        if index > CHAIN_LEN {
            return Err(Divergence::ChainExhausted);
        }
        let previous = match index {
            1 => self.their_commit.ok_or(Divergence::BadReveal { index })?,
            _ => self.their_values[index as usize - 2],
        };
        if chain_step(&value) != previous {
            return Err(Divergence::BadReveal { index });
        }
        self.their_values.push(value);
        Ok(())
    }

    /// Our chain value at `index`.
    fn value(&self, index: u32) -> Hash {
        self.chain[index as usize]
    }

    fn status(&self, rules: &impl Rules) -> Result<status::State, CallError> {
        let status = rules.status(&self.state)?;
        status
            .state
            .ok_or(CallError::Malformed("Status without a state"))
    }

    fn send(&mut self, body: Body) -> Message {
        let reveals = (self.revealed + 1..=self.needed)
            .map(|k| self.value(k))
            .collect();
        self.revealed = self.needed;
        let message = Message {
            match_id: self.id,
            n: self.sent.len() as u32 + 1,
            reveals,
            body,
        };
        self.sent.push(message.clone());
        message
    }

    /// Decides the match from the resignations made so far. A resignation ends it even
    /// if the game had ended later on the other side's board: a resignation at ply `p` was
    /// made before that player saw move `p + 1`, and both sides apply this same rule.
    fn conclude_resignations(&mut self) {
        let resigned =
            |side: usize| self.resigned[side].filter(|&p| self.ended_at.is_none_or(|end| p < end));
        let outcome = match (resigned(0), resigned(1)) {
            (Some(a), Some(b)) if a == b => Outcome {
                winner: None,
                reason: Reason::BothResigned,
            },
            (Some(a), Some(b)) => Outcome {
                winner: Some(if a < b { 1 } else { 0 }),
                reason: Reason::Resignation,
            },
            (Some(_), None) => Outcome {
                winner: Some(1),
                reason: Reason::Resignation,
            },
            (None, Some(_)) => Outcome {
                winner: Some(0),
                reason: Reason::Resignation,
            },
            (None, None) => return,
        };
        self.draw_offer = None;
        self.phase = Phase::Finished(outcome);
    }

    fn finish(&mut self, winner: Option<u32>, reason: Reason) {
        self.phase = Phase::Finished(Outcome { winner, reason });
    }

    fn diverge(&mut self, divergence: Divergence) {
        self.phase = Phase::Diverged(divergence);
    }
}

fn hash(domain: &[u8], parts: &[&[u8]]) -> Hash {
    let mut hasher = Sha256::new();
    hasher.update(domain);
    for part in parts {
        hasher.update(part);
    }
    hasher.finalize().into()
}

pub fn state_hash(state: &[u8]) -> Hash {
    hash(b"construct-game-state", &[state])
}

fn chain_step(value: &Hash) -> Hash {
    hash(b"construct-game-chain", &[value])
}

/// `[s_0, s_1, …, s_N]` from `s_N = secret`, with `s_{k-1} = H(s_k)`.
fn chain(secret: Hash) -> Vec<Hash> {
    let mut chain = vec![secret; CHAIN_LEN as usize + 1];
    for k in (0..CHAIN_LEN as usize).rev() {
        chain[k] = chain_step(&chain[k + 1]);
    }
    chain
}

fn seed(id: MatchId, inviter: &Hash, invitee: &Hash) -> [u8; SEED_LEN] {
    hash(b"construct-game-seed", &[&id.0, inviter, invitee])
}

/// The dice for roll `index`, uniform on `1..=sides` for each die: rejection sampling over
/// a stream of 32-bit words expanded from the roll's hash.
fn dice(id: MatchId, index: u32, v0: &Hash, v1: &Hash, sides: &[u32]) -> Vec<u32> {
    let roll = hash(
        b"construct-game-chance",
        &[&id.0, &index.to_le_bytes(), v0, v1],
    );
    let mut words = (0u32..).flat_map(|block| {
        let bytes = hash(b"construct-game-dice", &[&roll, &block.to_le_bytes()]);
        (0..8).map(move |i| u32::from_le_bytes(bytes[i * 4..i * 4 + 4].try_into().unwrap()))
    });
    sides
        .iter()
        .map(|&sides| {
            let sides = sides.max(1);
            let limit = u32::MAX - (u32::MAX % sides);
            loop {
                let word = words.next().expect("endless");
                if word < limit {
                    return word % sides + 1;
                }
            }
        })
        .collect()
}

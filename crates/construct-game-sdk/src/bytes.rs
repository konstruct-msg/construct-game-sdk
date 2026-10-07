//! The ABI at the level of bytes: what each `cg_*` export does, written as ordinary
//! functions so native tests can call exactly what the module runs.
//!
//! `None` means the module failed — the host was given bytes it never produced (a state
//! that does not decode) — and is distinct from an [`Invalid`] move, which is an answer.

use alloc::vec::Vec;

use construct_game_abi::{
    ApplyResult, ChanceRoll, Message, MoveList, MoveOption, apply_result, status,
};

use crate::{Codec, Game, Invalid, PLAYER_CHANCE, SEED_LEN, invalid};

fn encode_result<S: Codec>(result: Result<S, Invalid>) -> Vec<u8> {
    let result = match result {
        Ok(state) => apply_result::Result::State(state.encode()),
        Err(invalid) => apply_result::Result::Invalid(invalid),
    };
    ApplyResult {
        result: Some(result),
    }
    .encode_to_vec()
}

pub fn init<G: Game>(seed: &[u8], options: &[u8]) -> Option<Vec<u8>> {
    let seed: &[u8; SEED_LEN] = seed.try_into().ok()?;
    let result = match G::Options::decode(options) {
        Some(options) => G::init(seed, options),
        None => Err(invalid::sdk(invalid::OPTIONS, "game.invalid.options")),
    };
    Some(encode_result(result))
}

pub fn apply<G: Game>(state: &[u8], mv: &[u8], player: u32) -> Option<Vec<u8>> {
    let state = G::State::decode(state)?;
    Some(encode_result(checked_apply::<G>(&state, mv, player)))
}

/// Turn order is the SDK's, not each game's: a move reaches the game only from the player
/// the game itself says is to move, and a roll only when it asked for one.
fn checked_apply<G: Game>(state: &G::State, mv: &[u8], player: u32) -> Result<G::State, Invalid> {
    match G::status(state).state {
        Some(status::State::ToMove(to_move)) if to_move == player && player != PLAYER_CHANCE => {
            let mv = G::Move::decode(mv)
                .ok_or_else(|| invalid::sdk(invalid::MOVE_ENCODING, "game.invalid.move"))?;
            G::apply(state, &mv, player)
        }
        Some(status::State::AwaitingChance(spec)) if player == PLAYER_CHANCE => {
            let roll = ChanceRoll::decode(mv)
                .ok()
                .filter(|roll| roll_fits(roll, &spec.dice))
                .ok_or_else(|| invalid::sdk(invalid::CHANCE_ROLL, "game.invalid.chance"))?;
            G::apply_chance(state, &roll)
        }
        Some(status::State::Finished(_)) => {
            Err(invalid::sdk(invalid::FINISHED, "game.invalid.finished"))
        }
        _ => Err(invalid::sdk(
            invalid::NOT_YOUR_TURN,
            "game.invalid.not_your_turn",
        )),
    }
}

fn roll_fits(roll: &ChanceRoll, dice: &[u32]) -> bool {
    roll.values.len() == dice.len()
        && roll
            .values
            .iter()
            .zip(dice)
            .all(|(&value, &sides)| (1..=sides).contains(&value))
}

pub fn legal_moves<G: Game>(state: &[u8], player: u32) -> Option<Vec<u8>> {
    let state = G::State::decode(state)?;
    let moves = G::legal_moves(&state, player)
        .into_iter()
        .map(|m| MoveOption {
            r#move: m.mv.encode(),
            from: m.from,
            to: m.to,
            choice_key: m.choice_key,
            action_key: m.action_key,
        })
        .collect();
    Some(MoveList { moves }.encode_to_vec())
}

pub fn view<G: Game>(state: &[u8], player: u32) -> Option<Vec<u8>> {
    let state = G::State::decode(state)?;
    Some(G::view(&state, player).encode_to_vec())
}

pub fn status<G: Game>(state: &[u8]) -> Option<Vec<u8>> {
    let state = G::State::decode(state)?;
    Some(G::status(&state).encode_to_vec())
}

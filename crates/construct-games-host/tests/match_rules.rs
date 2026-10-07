//! The match protocol's rules one at a time: what a tampered or out-of-place message
//! does, when chain values are revealed, which local actions are refused — and one whole
//! match through a real module.

mod common;

use std::path::Path;

use common::{DiceRace, Native, Rng};
use construct_game_abi::status;
use construct_games_host::game_match::{
    Body, Divergence, Match, MatchId, Message, Phase, PlayError, Reason,
};
use construct_games_host::{GameModule, Limits, Rules};
use tictactoe::TicTacToe;

fn ttt() -> Native<TicTacToe> {
    Native::new("tictactoe")
}

/// Delivers messages in order until neither side has anything more to say.
fn deliver(rules: &impl Rules, sides: &mut [Match; 2], mut queue: Vec<(usize, Message)>) {
    while !queue.is_empty() {
        let (to, message) = queue.remove(0);
        let replies = sides[to].receive(rules, message);
        queue.extend(replies.into_iter().map(|m| (1 - to, m)));
    }
}

/// An accepted match, both sides playing.
fn started(rules: &impl Rules, seed: u64) -> [Match; 2] {
    let mut rng = Rng(seed);
    let (inviter, invite) = Match::invite(rules, MatchId(rng.bytes()), Vec::new(), rng.bytes());
    let mut invitee = Match::invited(&invite, rng.bytes()).unwrap();
    let accept = invitee.accept(rules).unwrap();
    let mut sides = [inviter, invitee];
    deliver(
        rules,
        &mut sides,
        accept.into_iter().map(|m| (0, m)).collect(),
    );
    assert_eq!(sides[0].phase(), &Phase::Playing);
    assert_eq!(sides[1].phase(), &Phase::Playing);
    sides
}

/// The side that holds the move. Not simply "who is to move on side 0's board": after a
/// roll, the side whose turn it is knows the roll first and the other side learns it from
/// the move — one message per roll — so the other board may still be waiting for it.
fn mover(rules: &impl Rules, sides: &[Match; 2]) -> usize {
    sides
        .iter()
        .position(|m| matches!(rules.status(m.state()).unwrap().state, Some(status::State::ToMove(p)) if p == m.side()))
        .expect("one side holds the move")
}

/// The side to move plays `cell`; returns its message, not delivered.
fn ttt_move(sides: &mut [Match; 2], cell: u8) -> (usize, Message) {
    let rules = ttt();
    let mover = mover(&rules, sides);
    let mut out = sides[mover].play(&rules, &[cell]).unwrap();
    assert_eq!(out.len(), 1);
    (mover, out.remove(0))
}

fn diverged(m: &Match) -> &Divergence {
    match m.phase() {
        Phase::Diverged(d) => d,
        other => panic!("expected a divergence, got {other:?}"),
    }
}

fn with_body(message: &Message, body: Body) -> Message {
    Message {
        body,
        ..message.clone()
    }
}

// ── tampering ────────────────────────────────────────────────────────────────────

#[test]
fn a_wrong_state_hash_diverges() {
    let rules = ttt();
    let mut sides = started(&rules, 1);
    let (mover, message) = ttt_move(&mut sides, 4);
    let Body::Move { ply, mv, .. } = message.body.clone() else {
        unreachable!()
    };
    let forged = with_body(
        &message,
        Body::Move {
            ply,
            mv,
            state_hash: [7; 32],
        },
    );
    sides[1 - mover].receive(&rules, forged);
    assert!(
        matches!(diverged(&sides[1 - mover]), Divergence::StateHash { ply: 0, theirs, .. } if *theirs == [7; 32])
    );
}

#[test]
fn a_substituted_move_diverges_on_the_hash() {
    let rules = ttt();
    let mut sides = started(&rules, 2);
    let (mover, message) = ttt_move(&mut sides, 4);
    let Body::Move {
        ply, state_hash, ..
    } = message.body.clone()
    else {
        unreachable!()
    };
    // A legal move, but not the one the hash was computed for.
    let forged = with_body(
        &message,
        Body::Move {
            ply,
            mv: vec![0],
            state_hash,
        },
    );
    sides[1 - mover].receive(&rules, forged);
    assert!(matches!(
        diverged(&sides[1 - mover]),
        Divergence::StateHash { .. }
    ));
}

#[test]
fn an_illegal_move_diverges() {
    let rules = ttt();
    let mut sides = started(&rules, 3);
    let (a, first) = ttt_move(&mut sides, 4);
    deliver(&rules, &mut sides, vec![(1 - a, first)]);
    let (b, reply) = ttt_move(&mut sides, 0);
    deliver(&rules, &mut sides, vec![(1 - b, reply)]);
    // `a` again, onto the cell it already holds.
    let n = sides[a].sent().len() as u32 + 1;
    let forged = Message {
        match_id: sides[a].id(),
        n,
        reveals: Vec::new(),
        body: Body::Move {
            ply: 2,
            mv: vec![4],
            state_hash: [0; 32],
        },
    };
    sides[b].receive(&rules, forged);
    assert!(matches!(
        diverged(&sides[b]),
        Divergence::IllegalMove { ply: 2, .. }
    ));
}

#[test]
fn a_move_out_of_turn_diverges() {
    let rules = ttt();
    let mut sides = started(&rules, 4);
    let (a, first) = ttt_move(&mut sides, 4);
    deliver(&rules, &mut sides, vec![(1 - a, first.clone())]);
    // `a` moves again before the other side has.
    let forged = Message {
        n: first.n + 1,
        body: Body::Move {
            ply: 1,
            mv: vec![0],
            state_hash: [0; 32],
        },
        ..first
    };
    sides[1 - a].receive(&rules, forged);
    assert_eq!(
        diverged(&sides[1 - a]),
        &Divergence::NotTheirTurn { ply: 1 }
    );
}

#[test]
fn a_move_for_another_ply_diverges() {
    let rules = ttt();
    let mut sides = started(&rules, 5);
    let (mover, message) = ttt_move(&mut sides, 4);
    let Body::Move { mv, state_hash, .. } = message.body.clone() else {
        unreachable!()
    };
    let forged = with_body(
        &message,
        Body::Move {
            ply: 3,
            mv,
            state_hash,
        },
    );
    sides[1 - mover].receive(&rules, forged);
    assert_eq!(
        diverged(&sides[1 - mover]),
        &Divergence::Ply {
            expected: 0,
            got: 3
        }
    );
}

#[test]
fn a_tampered_chain_value_diverges() {
    let rules = Native::<DiceRace>::new("dicerace");
    let mut rng = Rng(6);
    let (inviter, invite) = Match::invite(&rules, MatchId(rng.bytes()), Vec::new(), rng.bytes());
    let mut invitee = Match::invited(&invite, rng.bytes()).unwrap();
    let mut accept = invitee.accept(&rules).unwrap().remove(0);
    assert_eq!(
        accept.reveals.len(),
        1,
        "Accept carries the invitee's value 1"
    );
    accept.reveals[0][0] ^= 1;
    let mut sides = [inviter, invitee];
    sides[0].receive(&rules, accept);
    assert_eq!(diverged(&sides[0]), &Divergence::BadReveal { index: 1 });
}

#[test]
fn a_tampered_roll_value_diverges() {
    let rules = Native::<DiceRace>::new("dicerace");
    // Find the first message that carries a value past the seed, and corrupt it.
    let mut sides = started(&rules, 7);
    for round in 0..200 {
        let mover = mover(&rules, &sides);
        let mut out = sides[mover].play(&rules, &[0]).unwrap();
        if let Some(message) = out.iter_mut().find(|m| !m.reveals.is_empty()) {
            let index = sides[1 - mover].rolls_spent() + 1;
            message.reveals[0][31] ^= 0x80;
            let message = message.clone();
            sides[1 - mover].receive(&rules, message);
            assert_eq!(
                diverged(&sides[1 - mover]),
                &Divergence::BadReveal { index }
            );
            return;
        }
        deliver(
            &rules,
            &mut sides,
            out.into_iter().map(|m| (1 - mover, m)).collect(),
        );
        assert!(round < 199, "no roll value was ever carried by a move");
    }
}

#[test]
fn the_same_number_with_other_content_diverges() {
    let rules = ttt();
    let mut sides = started(&rules, 8);
    let (mover, message) = ttt_move(&mut sides, 4);
    sides[1 - mover].receive(&rules, message.clone());
    let other = with_body(&message, Body::Resign { ply: 0 });
    sides[1 - mover].receive(&rules, other);
    assert_eq!(
        diverged(&sides[1 - mover]),
        &Divergence::Conflict { n: message.n }
    );
}

#[test]
fn repeats_are_dropped_and_order_is_restored() {
    let rules = ttt();
    let mut sides = started(&rules, 9);
    let (a, first) = ttt_move(&mut sides, 4);
    deliver(&rules, &mut sides, vec![(1 - a, first)]);
    let (b, reply) = ttt_move(&mut sides, 0);
    deliver(&rules, &mut sides, vec![(1 - b, reply)]);
    let (_, second) = ttt_move(&mut sides, 8);
    let (_, offer) = (a, sides[a].sent().last().unwrap().clone());
    assert_eq!(offer, second);
    // `a`'s third and fourth messages, delivered fourth-first and each twice.
    let third = second.clone();
    let mut sides_b = sides;
    let resign = sides_b[a].resign(&rules).unwrap().remove(0);
    for message in [resign.clone(), third.clone(), resign, third] {
        sides_b[b].receive(&rules, message);
    }
    assert_eq!(
        sides_b[b].ply(),
        3,
        "the move was applied once, before the resignation"
    );
    let Phase::Finished(outcome) = sides_b[b].phase() else {
        panic!("{:?}", sides_b[b].phase())
    };
    assert_eq!(outcome.winner, Some(b as u32));
    assert_eq!(outcome.reason, Reason::Resignation);
}

// ── chain values ─────────────────────────────────────────────────────────────────

/// A side's value for roll `k` must not leave it before its own game asks for roll `k`:
/// with it, the other side would know that roll while still choosing its moves. Checked
/// after every single step, on each side against its own board — after full delivery
/// the roll has been made and an early value no longer shows.
#[test]
fn values_leave_only_for_rolls_the_game_has_asked_for() {
    let rules = Native::<DiceRace>::new("dicerace");
    let check = |sides: &[Match; 2]| {
        for side in sides {
            let revealed: u32 = side.sent().iter().map(|m| m.reveals.len() as u32).sum();
            assert!(
                revealed <= side.rolls_spent() + 1,
                "side {} revealed {revealed} values with {} rolls spent",
                side.side(),
                side.rolls_spent(),
            );
        }
    };
    for seed in 0..50 {
        let mut rng = Rng(seed + 100);
        let mut sides = started(&rules, seed + 100);
        check(&sides);
        while sides[0].phase() == &Phase::Playing || sides[1].phase() == &Phase::Playing {
            let mover = mover(&rules, &sides);
            let mv = [rng.below(2) as u8];
            let mut queue: Vec<_> = sides[mover]
                .play(&rules, &mv)
                .unwrap()
                .into_iter()
                .map(|m| (1 - mover, m))
                .collect();
            check(&sides);
            while !queue.is_empty() {
                let (to, message) = queue.remove(0);
                let replies = sides[to].receive(&rules, message);
                queue.extend(replies.into_iter().map(|m| (1 - to, m)));
                check(&sides);
            }
        }
        assert!(sides[0].rolls_spent() > 2, "the game asked for rolls");
        assert_eq!(sides[0].phase(), sides[1].phase());
        assert_eq!(sides[0].state(), sides[1].state());
    }
}

#[test]
fn both_sides_compute_the_same_rolls() {
    let rules = Native::<DiceRace>::new("dicerace");
    let mut sides = started(&rules, 11);
    for _ in 0..10 {
        if sides[0].phase() != &Phase::Playing {
            break;
        }
        let mover = mover(&rules, &sides);
        let out = sides[mover].play(&rules, &[0]).unwrap();
        deliver(
            &rules,
            &mut sides,
            out.into_iter().map(|m| (1 - mover, m)).collect(),
        );
        // The side that will move after a roll learns it first; the other learns it from
        // that move. So the boards agree at equal rolls, and one may lag by exactly the
        // roll it is waiting for.
        let (a, b) = (sides[0].rolls_spent(), sides[1].rolls_spent());
        if a == b {
            assert_eq!(sides[0].state(), sides[1].state());
        } else {
            let behind = if a < b { 0 } else { 1 };
            assert_eq!(a.abs_diff(b), 1);
            let waiting = rules.status(sides[behind].state()).unwrap().state;
            assert!(
                matches!(waiting, Some(status::State::AwaitingChance(_))),
                "{waiting:?}"
            );
        }
    }
    assert!(sides[0].rolls_spent() > 3);
}

// ── local actions ────────────────────────────────────────────────────────────────

#[test]
fn local_actions_are_refused_without_sending_anything() {
    let rules = ttt();
    let mut sides = started(&rules, 12);
    let mover = mover(&rules, &sides);
    let waiting = 1 - mover;
    let sent_before = sides[waiting].sent().len();

    assert_eq!(
        sides[waiting].play(&rules, &[4]),
        Err(PlayError::NotYourTurn)
    );
    assert!(matches!(
        sides[mover].play(&rules, &[9]),
        Err(PlayError::Invalid(_))
    ));
    assert_eq!(
        sides[waiting].answer_draw(&rules, true),
        Err(PlayError::NoOfferToAnswer)
    );
    let other_game = Native::<DiceRace>::new("dicerace");
    assert_eq!(
        sides[mover].play(&other_game, &[4]),
        Err(PlayError::WrongGame)
    );

    // While its offer stands, a side can neither move nor resign.
    let offer = sides[mover].offer_draw(&rules).unwrap();
    assert_eq!(
        sides[mover].play(&rules, &[4]),
        Err(PlayError::OfferPending)
    );
    assert_eq!(sides[mover].resign(&rules), Err(PlayError::OfferPending));
    assert_eq!(sides[waiting].sent().len(), sent_before);

    deliver(
        &rules,
        &mut sides,
        offer.into_iter().map(|m| (waiting, m)).collect(),
    );
    let answer = sides[waiting].answer_draw(&rules, false).unwrap();
    deliver(
        &rules,
        &mut sides,
        answer.into_iter().map(|m| (mover, m)).collect(),
    );
    assert!(
        sides[mover].play(&rules, &[4]).is_ok(),
        "a declined offer frees the side to move"
    );
}

#[test]
fn offers_crossing_for_the_same_position_agree_a_draw() {
    let rules = ttt();
    let mut sides = started(&rules, 13);
    let a = sides[0].offer_draw(&rules).unwrap();
    let b = sides[1].offer_draw(&rules).unwrap();
    deliver(
        &rules,
        &mut sides,
        a.into_iter()
            .map(|m| (1, m))
            .chain(b.into_iter().map(|m| (0, m)))
            .collect(),
    );
    for side in &sides {
        assert_eq!(
            side.phase(),
            &Phase::Finished(construct_games_host::game_match::Outcome {
                winner: None,
                reason: Reason::DrawAgreed
            })
        );
    }
}

#[test]
fn a_move_declines_an_offer_it_crossed() {
    let rules = ttt();
    let mut sides = started(&rules, 14);
    let mover = mover(&rules, &sides);
    let offer = sides[1 - mover].offer_draw(&rules).unwrap();
    let mv = sides[mover].play(&rules, &[4]).unwrap();
    deliver(
        &rules,
        &mut sides,
        offer
            .into_iter()
            .map(|m| (mover, m))
            .chain(mv.into_iter().map(|m| (1 - mover, m)))
            .collect(),
    );
    for side in &sides {
        assert_eq!(side.phase(), &Phase::Playing);
        assert_eq!(side.draw_offer(), None);
    }
}

#[test]
fn resignations_crossing_at_the_same_position_are_a_draw() {
    let rules = ttt();
    let mut sides = started(&rules, 15);
    let a = sides[0].resign(&rules).unwrap();
    let b = sides[1].resign(&rules).unwrap();
    deliver(
        &rules,
        &mut sides,
        a.into_iter()
            .map(|m| (1, m))
            .chain(b.into_iter().map(|m| (0, m)))
            .collect(),
    );
    for side in &sides {
        let Phase::Finished(outcome) = side.phase() else {
            panic!()
        };
        assert_eq!(outcome.reason, Reason::BothResigned);
        assert_eq!(outcome.winner, None);
    }
}

#[test]
fn a_declined_invitation_ends_on_both_sides() {
    let rules = ttt();
    let (mut inviter, invite) = Match::invite(&rules, MatchId([1; 16]), Vec::new(), [2; 32]);
    let mut invitee = Match::invited(&invite, [3; 32]).unwrap();
    let decline = invitee.decline().unwrap().remove(0);
    inviter.receive(&rules, decline);
    assert_eq!(inviter.phase(), &Phase::Declined);
    assert_eq!(invitee.phase(), &Phase::Declined);
}

#[test]
fn accepting_with_another_game_is_refused() {
    let (_, invite) = Match::invite(&ttt(), MatchId([1; 16]), Vec::new(), [2; 32]);
    let mut invitee = Match::invited(&invite, [3; 32]).unwrap();
    assert_eq!(
        invitee.accept(&Native::<DiceRace>::new("dicerace")),
        Err(PlayError::WrongGame)
    );
    assert_eq!(invitee.phase(), &Phase::Invited);
}

// ── through a real module ────────────────────────────────────────────────────────

#[test]
fn a_whole_match_runs_through_the_built_module() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let wasm = std::fs::read(root.join("dist/tictactoe.wasm"))
        .expect("dist/tictactoe.wasm is missing — run scripts/build-games.sh --native");
    let module = GameModule::load(&wasm, Limits::default()).unwrap();
    let mut sides = started(&module, 16);
    let mut rng = Rng(17);
    while sides[0].phase() == &Phase::Playing {
        let mover = mover(&module, &sides);
        let moves = module
            .legal_moves(sides[mover].state(), mover as u32)
            .unwrap()
            .moves;
        let mv = moves[rng.below(moves.len())].r#move.clone();
        let out = sides[mover].play(&module, &mv).unwrap();
        deliver(
            &module,
            &mut sides,
            out.into_iter().map(|m| (1 - mover, m)).collect(),
        );
    }
    assert!(
        matches!(sides[0].phase(), Phase::Finished(o) if matches!(o.reason, Reason::Game { .. }))
    );
    assert_eq!(sides[0].phase(), sides[1].phase());
    assert_eq!(sides[0].state(), sides[1].state());
}

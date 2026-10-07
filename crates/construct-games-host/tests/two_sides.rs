//! Two sides of a match in one process, over a network that loses, repeats and reorders.
//! Both players act at random — moves, resignations, draw offers and answers, crossing
//! each other in flight. Every match must end the same on both sides, never diverge,
//! and, when the game itself ended it, on the same state.

mod common;

use common::{DiceRace, Native, Rng};
use construct_game_abi::{MoveList, status};
use construct_games_host::Rules;
use construct_games_host::game_match::{Match, MatchId, Message, Phase, Reason};
use tictactoe::TicTacToe;

const MATCHES: u32 = 10_000;
/// A match that has not ended after this many steps is stuck.
const STEP_LIMIT: u32 = 20_000;

struct Network {
    /// `(recipient, message)`; the inviter is 0.
    in_flight: Vec<(usize, Message)>,
}

fn legal(rules: &impl Rules, m: &Match) -> MoveList {
    rules.legal_moves(m.state(), m.side()).unwrap()
}

fn is_my_turn(rules: &impl Rules, m: &Match) -> bool {
    m.phase() == &Phase::Playing
        && matches!(rules.status(m.state()).unwrap().state, Some(status::State::ToMove(p)) if p == m.side())
}

/// How often, per thousand actions, a player resigns or offers a draw instead of moving.
/// A long game needs these rare, or almost no match reaches the game's own end.
#[derive(Clone, Copy)]
struct Temper {
    resign: u64,
    offer: u64,
}

/// One random action by `side`, if it has one: answer an offer, or on its turn move,
/// now and then resign or offer a draw — on its turn or not.
fn act(rules: &impl Rules, rng: &mut Rng, temper: Temper, m: &mut Match) -> Vec<Message> {
    if m.phase() != &Phase::Playing {
        return Vec::new();
    }
    if let Some((who, _)) = m.draw_offer()
        && who != m.side()
        && rng.chance(50)
    {
        return m.answer_draw(rules, rng.chance(50)).unwrap();
    }
    if rng.below(1000) < temper.resign as usize {
        return m.resign(rules).unwrap_or_default();
    }
    if rng.below(1000) < temper.offer as usize {
        return m.offer_draw(rules).unwrap_or_default();
    }
    if is_my_turn(rules, m) && m.draw_offer().is_none_or(|(who, _)| who != m.side()) {
        let moves = legal(rules, m).moves;
        let mv = moves[rng.below(moves.len())].r#move.clone();
        return m.play(rules, &mv).unwrap();
    }
    Vec::new()
}

fn done(m: &Match) -> bool {
    !matches!(
        m.phase(),
        Phase::Playing | Phase::Inviting | Phase::Invited | Phase::AwaitingBegin
    )
}

fn play_matches(rules: &impl Rules, seed: u64, temper: Temper) -> [u32; 4] {
    let mut rng = Rng(seed);
    // [finished by the game, by resignation, by agreement, both resigned]
    let mut tally = [0u32; 4];

    for number in 0..MATCHES {
        let id = MatchId(rng.bytes());
        let (inviter, invite) = Match::invite(rules, id, Vec::new(), rng.bytes());
        let mut invitee = Match::invited(&invite, rng.bytes()).unwrap();
        let mut net = Network {
            in_flight: invitee
                .accept(rules)
                .unwrap()
                .into_iter()
                .map(|m| (0, m))
                .collect(),
        };
        let mut sides = [inviter, invitee];

        let mut steps = 0;
        while !(done(&sides[0]) && done(&sides[1])) {
            steps += 1;
            assert!(
                steps < STEP_LIMIT,
                "match {number} stuck: {:?} / {:?}",
                sides[0].phase(),
                sides[1].phase()
            );

            if net.in_flight.is_empty() || rng.chance(3) {
                // A side resends everything it ever sent, as a client would after a loss.
                let from = rng.below(2);
                let copies: Vec<_> = sides[from]
                    .sent()
                    .iter()
                    .map(|m| (1 - from, m.clone()))
                    .collect();
                net.in_flight.extend(copies);
                continue;
            }
            match rng.below(10) {
                0 => {
                    net.in_flight.swap_remove(rng.below(net.in_flight.len()));
                }
                1..=5 => {
                    let i = rng.below(net.in_flight.len());
                    let (to, message) = if rng.chance(10) {
                        net.in_flight[i].clone() // delivered, and still in flight: a repeat
                    } else {
                        net.in_flight.swap_remove(i)
                    };
                    let replies = sides[to].receive(rules, message);
                    net.in_flight
                        .extend(replies.into_iter().map(|m| (1 - to, m)));
                }
                _ => {
                    let who = rng.below(2);
                    let out = act(rules, &mut rng, temper, &mut sides[who]);
                    net.in_flight.extend(out.into_iter().map(|m| (1 - who, m)));
                }
            }
            for side in &sides {
                assert!(
                    !matches!(side.phase(), Phase::Diverged(_)),
                    "match {number} diverged on side {}: {:?}",
                    side.side(),
                    side.phase(),
                );
            }
        }

        // Both sides have stopped, but messages may still be lost in flight — say the
        // resignation that crossed the other side's. Agreement is promised only once
        // everything sent has arrived, so deliver it all, as clients resending would.
        loop {
            let before: usize = sides.iter().map(|s| s.sent().len()).sum();
            for from in 0..2 {
                for message in sides[from].sent().to_vec() {
                    sides[1 - from].receive(rules, message);
                }
            }
            if sides.iter().map(|s| s.sent().len()).sum::<usize>() == before {
                break;
            }
        }
        for side in &sides {
            assert!(
                !matches!(side.phase(), Phase::Diverged(_)),
                "match {number}: {:?}",
                side.phase()
            );
        }

        let (Phase::Finished(a), Phase::Finished(b)) = (sides[0].phase(), sides[1].phase()) else {
            panic!(
                "match {number} ended {:?} / {:?}",
                sides[0].phase(),
                sides[1].phase()
            );
        };
        assert_eq!(
            a, b,
            "match {number}: the two sides disagree on the outcome"
        );
        if let Reason::Game { .. } = a.reason {
            assert_eq!(
                sides[0].state(),
                sides[1].state(),
                "match {number}: final states differ"
            );
        }
        tally[match a.reason {
            Reason::Game { .. } => 0,
            Reason::Resignation => 1,
            Reason::DrawAgreed => 2,
            Reason::BothResigned => 3,
        }] += 1;
    }
    tally
}

#[test]
fn tic_tac_toe_matches_agree_over_a_lossy_network() {
    let temper = Temper {
        resign: 10,
        offer: 20,
    };
    let tally = play_matches(
        &Native::<TicTacToe>::new("tictactoe"),
        0x1234_5678_9ABC_DEF1,
        temper,
    );
    eprintln!(
        "tic-tac-toe: game {}, resigned {}, draw agreed {}, both resigned {}",
        tally[0], tally[1], tally[2], tally[3]
    );
    assert!(
        tally.iter().all(|&n| n > 0),
        "every ending was reached: {tally:?}"
    );
}

#[test]
fn dice_matches_agree_over_a_lossy_network() {
    let temper = Temper {
        resign: 1,
        offer: 2,
    };
    let tally = play_matches(
        &Native::<DiceRace>::new("dicerace"),
        0x0FED_CBA9_8765_4321,
        temper,
    );
    eprintln!(
        "dice race: game {}, resigned {}, draw agreed {}, both resigned {}",
        tally[0], tally[1], tally[2], tally[3]
    );
    assert!(
        tally[0] > 0 && tally[1] > 0 && tally[2] > 0,
        "endings reached: {tally:?}"
    );
}

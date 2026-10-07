//! construct-game-check [--games N] [--full-games N] [--max-plies N] [--seed N]
//!                      [--options HEX] MODULE.wasm…
//!
//! Loads each module as the host does and runs `check` on it. Exit 1 if any module fails
//! to load or has a finding.

use std::process::ExitCode;

use construct_game_check::{Config, check};
use construct_games_host::{GameModule, Limits};

fn usage() -> ExitCode {
    eprintln!(
        "usage: construct-game-check [--games N] [--full-games N] [--max-plies N] [--seed N] \
         [--options HEX] MODULE.wasm..."
    );
    ExitCode::from(2)
}

fn main() -> ExitCode {
    let mut config = Config::default();
    let mut modules = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut number = |name: &str| -> Option<u64> {
            let value = args.next()?;
            value.parse().ok().or_else(|| {
                eprintln!("{name}: not a number: {value}");
                None
            })
        };
        match arg.as_str() {
            "--games" => match number("--games") {
                Some(n) => config.games = n as u32,
                None => return usage(),
            },
            "--full-games" => match number("--full-games") {
                Some(n) => config.full_games = n as u32,
                None => return usage(),
            },
            "--max-plies" => match number("--max-plies") {
                Some(n) => config.max_plies = n as u32,
                None => return usage(),
            },
            "--seed" => match number("--seed") {
                Some(n) => config.seed = n,
                None => return usage(),
            },
            "--options" => match args.next().and_then(|hex| decode_hex(&hex)) {
                Some(bytes) => config.options = bytes,
                None => return usage(),
            },
            flag if flag.starts_with("--") => return usage(),
            path => modules.push(path.to_string()),
        }
    }
    if modules.is_empty() {
        return usage();
    }

    let mut failed = false;
    for path in &modules {
        println!("== {path}");
        let wasm = match std::fs::read(path) {
            Ok(wasm) => wasm,
            Err(e) => {
                println!("FINDING cannot read: {e}");
                failed = true;
                continue;
            }
        };
        let module = match GameModule::load(&wasm, Limits::default()) {
            Ok(module) => module,
            Err(e) => {
                println!("FINDING refused at load: {e}");
                failed = true;
                continue;
            }
        };
        println!("id {:?}, {} bytes", module.id(), wasm.len());
        let report = check(&module, &config);
        print!("{report}");
        failed |= !report.passed();
    }
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn decode_hex(hex: &str) -> Option<Vec<u8>> {
    if !hex.len().is_multiple_of(2) {
        return None;
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok())
        .collect()
}

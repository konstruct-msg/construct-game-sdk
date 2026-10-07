//! Modules that break one rule each. Every fixture is the same small module with one
//! change, and `the_base_module_loads_and_answers` shows the unchanged one works — so
//! each refusal below is caused by its change and nothing else.

use construct_game_abi::{SEED_LEN, status};
use construct_games_host::{Applied, CallError, GameModule, Limits, LoadError};

/// Answers with fixed messages: `Status { to_move: 0 }` at 1024, `ApplyResult { state:
/// "x" }` at 1040, empty `MoveList`/`GameView`. `(ptr << 32) | len` in hex: 0x400 << 32
/// is 0x400_0000_0000.
const BASE: &str = r#"
(module
  ;;IMPORT
  (memory (export "memory") 1)
  (data (i32.const 1024) "\08\00")
  (data (i32.const 1040) "\0a\01\78")
  (data (i32.const 2048) "\ff\ff\ff")
  (global $heap (mut i32) (i32.const 4096))
  (global $calls (mut i32) (i32.const 0))
  (func (export "cg_abi_version") (result i32) (i32.const 1))
  (func (export "cg_alloc") (param $len i32) (result i32)
    (local $p i32)
    ;;ALLOC
    (local.set $p (global.get $heap))
    (global.set $heap (i32.add (global.get $heap) (local.get $len)))
    (local.get $p))
  (func (export "cg_init") (param i32 i32 i32 i32) (result i64) (i64.const 0x41000000003))
  (func (export "cg_apply") (param i32 i32 i32 i32 i32) (result i64) (i64.const 0x41000000003))
  (func (export "cg_legal_moves") (param i32 i32 i32) (result i64) (i64.const 0x40000000000))
  (func (export "cg_view") (param i32 i32 i32) (result i64) (i64.const 0x40000000000))
  (func (export "cg_status") (param i32 i32) (result i64)
    ;;STATUS
    (i64.const 0x40000000002))
  ;;EXTRA
)"#;

fn wasm(source: &str) -> Vec<u8> {
    wat::parse_str(source).expect("fixture is valid WAT")
}

fn with(marker: &str, code: &str) -> Vec<u8> {
    let marker = format!(";;{marker}");
    assert!(BASE.contains(&marker), "no marker {marker}");
    wasm(&BASE.replace(&marker, code))
}

fn replacing(from: &str, to: &str) -> Vec<u8> {
    assert!(BASE.contains(from), "fixture text {from:?} not in BASE");
    wasm(&BASE.replace(from, to))
}

fn load(wasm: &[u8]) -> Result<GameModule, LoadError> {
    GameModule::load(wasm, Limits::default())
}

fn status_error(wasm: &[u8]) -> CallError {
    load(wasm)
        .expect("loads")
        .status(b"state")
        .expect_err("status must fail")
}

#[test]
fn the_base_module_loads_and_answers() {
    let module = load(&wasm(BASE)).expect("the base module is valid");
    let status = module.status(b"state").unwrap();
    assert_eq!(status.state, Some(status::State::ToMove(0)));
    assert_eq!(
        module.init(&[0; SEED_LEN], &[]).unwrap(),
        Applied::State(b"x".to_vec())
    );
    assert_eq!(
        module.apply(b"s", b"m", 1).unwrap(),
        Applied::State(b"x".to_vec())
    );
    assert!(module.legal_moves(b"s", 0).unwrap().moves.is_empty());
    module.view(b"s", 0).unwrap();
}

// ── refused at load ──────────────────────────────────────────────────────────────

#[test]
fn an_import_is_refused() {
    let error = load(&with(
        "IMPORT",
        r#"(import "env" "now" (func (result i64)))"#,
    ))
    .err();
    assert_eq!(error, Some(LoadError::Imports(vec!["env.now".into()])));
}

#[test]
fn a_float_instruction_is_refused() {
    let code = "(func (result f64) (f64.add (f64.const 1) (f64.const 2)))";
    assert!(matches!(
        load(&with("EXTRA", code)),
        Err(LoadError::Rejected(_))
    ));
}

#[test]
fn a_float_type_alone_is_refused() {
    assert!(matches!(
        load(&with("EXTRA", "(func (param f32))")),
        Err(LoadError::Rejected(_))
    ));
}

#[test]
fn simd_is_refused() {
    let code = "(func (result v128) (v128.const i32x4 0 0 0 0))";
    assert!(matches!(
        load(&with("EXTRA", code)),
        Err(LoadError::Rejected(_))
    ));
}

#[test]
fn a_start_function_is_refused() {
    let code = "(func $start) (start $start)";
    assert!(matches!(
        load(&with("EXTRA", code)),
        Err(LoadError::Rejected(_))
    ));
}

#[test]
fn a_missing_export_is_refused() {
    let module = replacing(r#"(func (export "cg_view")"#, "(func");
    assert_eq!(
        load(&module).err(),
        Some(LoadError::MissingExport("cg_view"))
    );
}

#[test]
fn an_export_with_the_wrong_signature_is_refused() {
    let module = replacing(
        r#"(func (export "cg_apply") (param i32 i32 i32 i32 i32)"#,
        r#"(func (export "cg_apply") (param i32 i32 i32 i32)"#,
    );
    assert_eq!(
        load(&module).err(),
        Some(LoadError::WrongExportType("cg_apply"))
    );
}

#[test]
fn another_abi_version_is_refused() {
    let module = replacing("(result i32) (i32.const 1))", "(result i32) (i32.const 2))");
    assert_eq!(load(&module).err(), Some(LoadError::AbiVersion(2)));
}

#[test]
fn initial_memory_over_the_limit_is_refused() {
    // 512 pages = 32 MiB, twice the default limit.
    let module = replacing(
        r#"(memory (export "memory") 1)"#,
        r#"(memory (export "memory") 512)"#,
    );
    assert!(matches!(load(&module), Err(LoadError::Instantiate(_))));
}

#[test]
fn a_module_over_the_size_limit_is_refused() {
    let limits = Limits {
        module_bytes: 64,
        ..Limits::default()
    };
    assert!(matches!(
        GameModule::load(&wasm(BASE), limits),
        Err(LoadError::TooLarge { .. })
    ));
}

// ── refused per call ─────────────────────────────────────────────────────────────

#[test]
fn an_endless_loop_runs_out_of_fuel() {
    let module = with("STATUS", "(loop $spin (br $spin)) (unreachable)");
    assert_eq!(status_error(&module), CallError::OutOfFuel);
}

#[test]
fn growing_memory_without_end_hits_the_memory_limit() {
    let code = "(loop $grow (drop (memory.grow (i32.const 1))) (br $grow)) (unreachable)";
    assert_eq!(status_error(&with("STATUS", code)), CallError::MemoryLimit);
}

/// Unbounded recursion must end in wasmi's own call-depth limit — a trap — and never in
/// the host's native stack. (An endless loop once did overflow it: see `portable-dispatch`
/// in this crate's Cargo.toml.)
#[test]
fn endless_recursion_traps_inside_the_module() {
    let code = "(return (call $recurse))";
    let extra = "(func $recurse (result i64) (call $recurse))";
    let module = wasm(&BASE.replace(";;STATUS", code).replace(";;EXTRA", extra));
    assert!(matches!(status_error(&module), CallError::Trap(_)));
}

#[test]
fn a_trap_is_an_error_not_a_panic() {
    assert!(matches!(
        status_error(&with("STATUS", "(unreachable)")),
        CallError::Trap(_)
    ));
}

#[test]
fn zero_is_a_module_failure() {
    let module = with("STATUS", "(return (i64.const 0))");
    assert_eq!(status_error(&module), CallError::ModuleFailed);
}

#[test]
fn output_outside_memory_is_refused() {
    let module = with("STATUS", "(return (i64.const 0x7FFF000000000010))");
    assert_eq!(status_error(&module), CallError::OutputOutOfBounds);
}

#[test]
fn output_over_the_message_limit_is_refused() {
    // 2 MiB at 1024 — inside a 16 MiB limit only if memory were that big, but the length
    // check comes first.
    let module = with("STATUS", "(return (i64.const 0x40000200000))");
    assert!(matches!(
        status_error(&module),
        CallError::OutputTooLarge { .. }
    ));
}

#[test]
fn bytes_that_are_not_the_message_are_malformed() {
    let module = with("STATUS", "(return (i64.const 0x80000000003))");
    assert_eq!(status_error(&module), CallError::Malformed("Status"));
}

#[test]
fn an_empty_status_is_malformed() {
    let module = with("STATUS", "(return (i64.const 0x40000000000))");
    assert_eq!(
        status_error(&module),
        CallError::Malformed("Status without a state")
    );
}

#[test]
fn alloc_returning_null_is_refused() {
    let module = with("ALLOC", "(return (i32.const 0))");
    assert_eq!(status_error(&module), CallError::BadAlloc);
}

#[test]
fn alloc_returning_memory_the_input_does_not_fit_is_refused() {
    // One page is 65 536 bytes; the five-byte input from 65 534 runs past it.
    let module = with("ALLOC", "(return (i32.const 65534))");
    assert_eq!(status_error(&module), CallError::BadAlloc);
}

#[test]
fn an_input_over_the_message_limit_is_refused() {
    let module = load(&wasm(BASE)).unwrap();
    let state = vec![0u8; Limits::default().message_bytes + 1];
    assert!(matches!(
        module.status(&state),
        Err(CallError::InputTooLarge { .. })
    ));
}

/// The module counts its calls in a global and fails from the second one on. It never
/// sees a second call: every call is a fresh instance.
#[test]
fn nothing_a_module_keeps_survives_to_the_next_call() {
    let code = "
        (global.set $calls (i32.add (global.get $calls) (i32.const 1)))
        (if (i32.gt_u (global.get $calls) (i32.const 1)) (then (return (i64.const 0))))";
    let module = load(&with("STATUS", code)).unwrap();
    for _ in 0..3 {
        module
            .status(b"state")
            .expect("each call is the first in its instance");
    }
}

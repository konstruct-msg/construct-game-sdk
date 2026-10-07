//! Runs game modules.
//!
//! A module is checked once, at [`GameModule::load`], and refused if it imports anything,
//! has a start function, uses floating point or SIMD, lacks an export or names the wrong
//! ABI version. Every call afterwards runs in a **fresh instance** that is dropped when
//! the call returns: whatever a module keeps in memory or globals cannot reach the next
//! call, so a game cannot behave differently for two players because their histories of
//! calls differed. Each call is bounded by fuel and by a memory limit; running out of
//! either, trapping, or answering with bytes that do not decode is a [`CallError`] —
//! never a panic in the host, and never a move.
//!
//! This layer runs a game; it does not referee a match. Who may move, the order of
//! moves and the state hash are [`game_match`]'s, which checks them itself: a third-party
//! module need not be built with the SDK that checks turn order.

pub mod game_match;

use std::fmt;

use construct_game_abi::{
    ABI_VERSION, ApplyResult, GameView, Invalid, Message, MoveList, SEED_LEN, Status, apply_result,
};
use sha2::{Digest, Sha256};
use wasmi::{
    CompilationMode, Config, Engine, ExternType, Instance, Linker, Module, Store, StoreLimits,
    StoreLimitsBuilder, TrapCode, Val, ValType,
};

/// SHA-256 of the module's bytes — the game's id, carried in an invitation.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GameId(pub [u8; 32]);

impl fmt::Debug for GameId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.iter().try_for_each(|b| write!(f, "{b:02x}"))
    }
}

/// Bounds on one module.
///
/// The fuel figure was measured, not guessed (2026-10-07): the costliest call of any
/// game is chess's `legal_moves` in a position with 218 legal moves, the most a legal
/// chess position has — 757 k fuel, about 1 ms. 10 M is 13 times that, and
/// `construct-game-check` requires every game to stay under a tenth of it. It also bounds
/// what a module that loops costs per call: about 12 ms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub module_bytes: usize,
    pub fuel_per_call: u64,
    pub memory_bytes: usize,
    /// Each input (state, move, options) and each output.
    pub message_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            module_bytes: 4 << 20,
            fuel_per_call: 10_000_000,
            memory_bytes: 16 << 20,
            message_bytes: 1 << 20,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadError {
    TooLarge {
        bytes: usize,
        limit: usize,
    },
    /// Failed wasm validation, including the features the host turns off: floating
    /// point, SIMD and start functions.
    Rejected(String),
    Imports(Vec<String>),
    MissingExport(&'static str),
    WrongExportType(&'static str),
    AbiVersion(u32),
    /// The module could not be instantiated within the limits, e.g. its initial memory is
    /// larger than allowed.
    Instantiate(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallError {
    OutOfFuel,
    MemoryLimit,
    Trap(String),
    InputTooLarge {
        bytes: usize,
        limit: usize,
    },
    /// `cg_alloc` returned 0, or memory the input does not fit in.
    BadAlloc,
    /// The export returned 0: by the ABI, the module failed (e.g. a state it cannot decode).
    ModuleFailed,
    OutputOutOfBounds,
    OutputTooLarge {
        bytes: usize,
        limit: usize,
    },
    /// Output bytes that are not the message the ABI promises.
    Malformed(&'static str),
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl fmt::Display for CallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for LoadError {}
impl std::error::Error for CallError {}

/// The answer to `init` and `apply`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Applied {
    State(Vec<u8>),
    Invalid(Invalid),
}

impl Applied {
    /// Reads an encoded `ApplyResult`.
    pub fn decode(bytes: &[u8]) -> Result<Self, CallError> {
        applied(bytes)
    }
}

/// What a match needs from a game. [`GameModule`] is the implementation that runs a
/// module; tests implement it over a game's native code, so the match protocol can be
/// exercised for thousands of games without the cost of an instance per call.
pub trait Rules {
    fn id(&self) -> GameId;
    fn init(&self, seed: &[u8; SEED_LEN], options: &[u8]) -> Result<Applied, CallError>;
    fn apply(&self, state: &[u8], mv: &[u8], player: u32) -> Result<Applied, CallError>;
    fn legal_moves(&self, state: &[u8], player: u32) -> Result<MoveList, CallError>;
    fn status(&self, state: &[u8]) -> Result<Status, CallError>;
}

impl Rules for GameModule {
    fn id(&self) -> GameId {
        GameModule::id(self)
    }
    fn init(&self, seed: &[u8; SEED_LEN], options: &[u8]) -> Result<Applied, CallError> {
        GameModule::init(self, seed, options)
    }
    fn apply(&self, state: &[u8], mv: &[u8], player: u32) -> Result<Applied, CallError> {
        GameModule::apply(self, state, mv, player)
    }
    fn legal_moves(&self, state: &[u8], player: u32) -> Result<MoveList, CallError> {
        GameModule::legal_moves(self, state, player)
    }
    fn status(&self, state: &[u8]) -> Result<Status, CallError> {
        GameModule::status(self, state)
    }
}

/// The raw answer of one export, with the fuel it took.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    pub bytes: Vec<u8>,
    pub fuel: u64,
}

use ValType::{I32, I64};

const EXPORTS: &[(&str, &[ValType], &[ValType])] = &[
    ("cg_abi_version", &[], &[I32]),
    ("cg_alloc", &[I32], &[I32]),
    ("cg_init", &[I32, I32, I32, I32], &[I64]),
    ("cg_apply", &[I32, I32, I32, I32, I32], &[I64]),
    ("cg_legal_moves", &[I32, I32, I32], &[I64]),
    ("cg_view", &[I32, I32, I32], &[I64]),
    ("cg_status", &[I32, I32], &[I64]),
];

struct HostState {
    limits: StoreLimits,
}

pub struct GameModule {
    id: GameId,
    engine: Engine,
    module: Module,
    limits: Limits,
}

impl GameModule {
    pub fn load(wasm: &[u8], limits: Limits) -> Result<Self, LoadError> {
        if wasm.len() > limits.module_bytes {
            return Err(LoadError::TooLarge {
                bytes: wasm.len(),
                limit: limits.module_bytes,
            });
        }

        let mut config = Config::default();
        // Eager: the whole module is validated and translated here, so nothing about it
        // can first go wrong in the middle of a match.
        config
            .floats(false)
            .allow_start_fn(false)
            .consume_fuel(true)
            .compilation_mode(CompilationMode::Eager);
        let engine = Engine::new(&config);
        let module = Module::new(&engine, wasm).map_err(|e| LoadError::Rejected(e.to_string()))?;

        let imports: Vec<String> = module
            .imports()
            .map(|i| format!("{}.{}", i.module(), i.name()))
            .collect();
        if !imports.is_empty() {
            return Err(LoadError::Imports(imports));
        }
        check_exports(&module)?;

        let loaded = Self {
            id: GameId(Sha256::digest(wasm).into()),
            engine,
            module,
            limits,
        };
        let version = loaded
            .abi_version()
            .map_err(|e| LoadError::Instantiate(e.to_string()))?;
        if version != ABI_VERSION {
            return Err(LoadError::AbiVersion(version));
        }
        Ok(loaded)
    }

    pub fn id(&self) -> GameId {
        self.id
    }

    pub fn init(&self, seed: &[u8; SEED_LEN], options: &[u8]) -> Result<Applied, CallError> {
        let output = self.call("cg_init", &[seed, options], None)?;
        applied(&output.bytes)
    }

    pub fn apply(&self, state: &[u8], mv: &[u8], player: u32) -> Result<Applied, CallError> {
        let output = self.call("cg_apply", &[state, mv], Some(player))?;
        applied(&output.bytes)
    }

    pub fn legal_moves(&self, state: &[u8], player: u32) -> Result<MoveList, CallError> {
        let output = self.call("cg_legal_moves", &[state], Some(player))?;
        MoveList::decode(output.bytes.as_slice()).map_err(|_| CallError::Malformed("MoveList"))
    }

    pub fn view(&self, state: &[u8], player: u32) -> Result<GameView, CallError> {
        let output = self.call("cg_view", &[state], Some(player))?;
        GameView::decode(output.bytes.as_slice()).map_err(|_| CallError::Malformed("GameView"))
    }

    pub fn status(&self, state: &[u8]) -> Result<Status, CallError> {
        let output = self.call("cg_status", &[state], None)?;
        let status =
            Status::decode(output.bytes.as_slice()).map_err(|_| CallError::Malformed("Status"))?;
        match status.state {
            Some(_) => Ok(status),
            None => Err(CallError::Malformed("Status without a state")),
        }
    }

    /// Runs one `cg_*` export that takes byte inputs and optionally a player: each input
    /// is copied in through `cg_alloc` and passed as a ptr/len pair, in order. Public for
    /// `construct-game-check`, which needs the fuel figure.
    pub fn call(
        &self,
        export: &str,
        inputs: &[&[u8]],
        player: Option<u32>,
    ) -> Result<Output, CallError> {
        let (mut store, instance) = self.instantiate()?;
        let memory = instance
            .get_memory(&store, "memory")
            .expect("load checked the memory export");
        let alloc = instance
            .get_typed_func::<u32, u32>(&store, "cg_alloc")
            .expect("load checked cg_alloc");

        let mut params = Vec::with_capacity(inputs.len() * 2 + 1);
        for input in inputs {
            if input.len() > self.limits.message_bytes {
                return Err(CallError::InputTooLarge {
                    bytes: input.len(),
                    limit: self.limits.message_bytes,
                });
            }
            let len = input.len() as u32;
            let ptr = alloc.call(&mut store, len).map_err(trap)?;
            if ptr == 0 && len > 0 {
                return Err(CallError::BadAlloc);
            }
            memory
                .write(&mut store, ptr as usize, input)
                .map_err(|_| CallError::BadAlloc)?;
            params.push(Val::I32(ptr as i32));
            params.push(Val::I32(len as i32));
        }
        if let Some(player) = player {
            params.push(Val::I32(player as i32));
        }

        let func = instance
            .get_func(&store, export)
            .expect("load checked the exports");
        let mut result = [Val::I64(0)];
        func.call(&mut store, &params, &mut result).map_err(trap)?;
        let fuel = self.limits.fuel_per_call - store.get_fuel().expect("fuel is on");

        let packed = match result[0] {
            Val::I64(0) => return Err(CallError::ModuleFailed),
            Val::I64(packed) => packed as u64,
            _ => unreachable!("load checked the result type"),
        };
        let (ptr, len) = ((packed >> 32) as usize, (packed & 0xFFFF_FFFF) as usize);
        if len > self.limits.message_bytes {
            return Err(CallError::OutputTooLarge {
                bytes: len,
                limit: self.limits.message_bytes,
            });
        }
        let mut bytes = vec![0u8; len];
        memory
            .read(&store, ptr, &mut bytes)
            .map_err(|_| CallError::OutputOutOfBounds)?;
        Ok(Output { bytes, fuel })
    }

    fn abi_version(&self) -> Result<u32, CallError> {
        let (mut store, instance) = self.instantiate()?;
        let version = instance
            .get_typed_func::<(), u32>(&store, "cg_abi_version")
            .expect("load checked cg_abi_version");
        version.call(&mut store, ()).map_err(trap)
    }

    fn instantiate(&self) -> Result<(Store<HostState>, Instance), CallError> {
        let limits = StoreLimitsBuilder::new()
            .memory_size(self.limits.memory_bytes)
            .memories(1)
            .tables(1)
            .instances(1)
            .trap_on_grow_failure(true)
            .build();
        let mut store = Store::new(&self.engine, HostState { limits });
        store.limiter(|state| &mut state.limits);
        store
            .set_fuel(self.limits.fuel_per_call)
            .expect("fuel is on");
        let instance = Linker::new(&self.engine)
            .instantiate_and_start(&mut store, &self.module)
            .map_err(trap)?;
        Ok((store, instance))
    }
}

fn check_exports(module: &Module) -> Result<(), LoadError> {
    let find = |name: &str| {
        module
            .exports()
            .find(|e| e.name() == name)
            .map(|e| e.ty().clone())
    };
    match find("memory") {
        Some(ExternType::Memory(_)) => {}
        Some(_) => return Err(LoadError::WrongExportType("memory")),
        None => return Err(LoadError::MissingExport("memory")),
    }
    for &(name, params, results) in EXPORTS {
        match find(name) {
            Some(ExternType::Func(ty)) if ty.params() == params && ty.results() == results => {}
            Some(_) => return Err(LoadError::WrongExportType(name)),
            None => return Err(LoadError::MissingExport(name)),
        }
    }
    Ok(())
}

fn trap(error: wasmi::Error) -> CallError {
    match error.as_trap_code() {
        Some(TrapCode::OutOfFuel) => CallError::OutOfFuel,
        Some(TrapCode::GrowthOperationLimited) => CallError::MemoryLimit,
        _ => CallError::Trap(error.to_string()),
    }
}

fn applied(bytes: &[u8]) -> Result<Applied, CallError> {
    let result = ApplyResult::decode(bytes).map_err(|_| CallError::Malformed("ApplyResult"))?;
    match result.result {
        Some(apply_result::Result::State(state)) => Ok(Applied::State(state)),
        Some(apply_result::Result::Invalid(invalid)) => Ok(Applied::Invalid(invalid)),
        None => Err(CallError::Malformed("ApplyResult without a result")),
    }
}

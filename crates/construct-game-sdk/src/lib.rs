//! Write a Konstruct game.
//!
//! A game is a pure state machine: implement [`Game`] for a type, call
//! [`export_game!`] on it, and build the crate for `wasm32-unknown-unknown`. The module
//! that comes out has no imports — no network, files, clock or randomness — and the host
//! refuses one that has any. Randomness reaches a game only as a move by
//! [`PLAYER_CHANCE`], answered from both players' revealed secrets.
//!
//! The game crate starts with
//! ```ignore
//! #![cfg_attr(target_arch = "wasm32", no_std)]
//! extern crate alloc;
//! ```
//! so it is `no_std` in the module (the macro supplies the allocator and panic handler)
//! and an ordinary crate in native tests. Use `alloc::collections::BTreeMap`, never a
//! hash map: iteration order must not depend on anything but the state.

#![no_std]

extern crate alloc;

pub mod bytes;

use alloc::string::String;
use alloc::vec::Vec;

pub use construct_game_abi as abi;
pub use construct_game_abi::{
    ABI_VERSION, ChanceRoll, ChanceSpec, GameView, Highlight, Invalid, Outcome, Piece, SEED_LEN,
    Status, Text,
};

pub const PLAYER_0: u32 = abi::Player::Player0 as u32;
pub const PLAYER_1: u32 = abi::Player::Player1 as u32;
pub const PLAYER_CHANCE: u32 = abi::Player::Chance as u32;

/// A game's own byte encoding of its state, moves and options.
///
/// For the state the encoding must be **canonical**: equal states encode to equal bytes.
/// Both players hash the state after every move and compare; two encodings of one state
/// would read as a desynchronised game.
pub trait Codec: Sized {
    fn encode(&self) -> Vec<u8>;
    fn decode(bytes: &[u8]) -> Option<Self>;
}

/// For a game with no options.
impl Codec for () {
    fn encode(&self) -> Vec<u8> {
        Vec::new()
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        bytes.is_empty().then_some(())
    }
}

/// A legal move and how the app offers it — see `MoveOption` in the ABI proto.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegalMove<M> {
    pub mv: M,
    pub from: Option<u32>,
    pub to: Option<u32>,
    pub choice_key: String,
    pub action_key: String,
}

impl<M> LegalMove<M> {
    /// Tap a target: go, tic-tac-toe.
    pub fn to(mv: M, to: u32) -> Self {
        Self {
            mv,
            from: None,
            to: Some(to),
            choice_key: String::new(),
            action_key: String::new(),
        }
    }

    /// Tap a piece, then a target: chess, backgammon.
    pub fn from_to(mv: M, from: u32, to: u32) -> Self {
        Self {
            mv,
            from: Some(from),
            to: Some(to),
            choice_key: String::new(),
            action_key: String::new(),
        }
    }

    /// A button: "pass", "done scoring".
    pub fn action(mv: M, action_key: impl Into<String>) -> Self {
        Self {
            mv,
            from: None,
            to: None,
            choice_key: String::new(),
            action_key: action_key.into(),
        }
    }

    /// Several moves share from/to; the app asks which.
    pub fn with_choice(mut self, choice_key: impl Into<String>) -> Self {
        self.choice_key = choice_key.into();
        self
    }
}

/// Reasons the SDK itself refuses a move, before the game sees it. Codes from
/// `0xFFFF_0000` up are the SDK's; a game uses codes below.
pub mod invalid {
    use super::Invalid;
    use alloc::string::ToString;

    pub const OPTIONS: u32 = 0xFFFF_0001;
    pub const MOVE_ENCODING: u32 = 0xFFFF_0002;
    pub const NOT_YOUR_TURN: u32 = 0xFFFF_0003;
    pub const FINISHED: u32 = 0xFFFF_0004;
    pub const CHANCE_ROLL: u32 = 0xFFFF_0005;

    pub(crate) fn sdk(code: u32, key: &str) -> Invalid {
        Invalid {
            code,
            reason_key: key.to_string(),
        }
    }
}

pub trait Game {
    type State: Codec;
    type Move: Codec;
    type Options: Codec;

    /// The seed is the match's first roll — both players' randomness — so it may decide
    /// who moves first.
    fn init(seed: &[u8; SEED_LEN], options: Self::Options) -> Result<Self::State, Invalid>;

    /// Called only when [`Game::status`] says `player` is to move; the SDK refuses the
    /// rest before they get here.
    fn apply(state: &Self::State, mv: &Self::Move, player: u32) -> Result<Self::State, Invalid>;

    /// Called only when [`Game::status`] is `AwaitingChance`, with a roll the SDK has
    /// already checked against that spec.
    fn apply_chance(state: &Self::State, roll: &ChanceRoll) -> Result<Self::State, Invalid> {
        let _ = (state, roll);
        Err(invalid::sdk(invalid::CHANCE_ROLL, "game.invalid.chance"))
    }

    /// Empty for a player who is not to move.
    fn legal_moves(state: &Self::State, player: u32) -> Vec<LegalMove<Self::Move>>;

    fn view(state: &Self::State, player: u32) -> GameView;

    fn status(state: &Self::State) -> Status;
}

/// Turns a [`Game`] into a module: the `cg_*` exports, an allocator and a panic handler,
/// all only when building for wasm32. A panic in the game traps; the host treats a trap
/// as a fault of the module, never as a move.
#[macro_export]
macro_rules! export_game {
    ($game:ty) => {
        #[cfg(target_arch = "wasm32")]
        mod __construct_game_exports {
            use super::*;
            use $crate::bytes;

            #[global_allocator]
            static ALLOC: $crate::__private::GlobalDlmalloc = $crate::__private::GlobalDlmalloc;

            #[panic_handler]
            fn panic(_: &core::panic::PanicInfo) -> ! {
                core::arch::wasm32::unreachable()
            }

            #[unsafe(no_mangle)]
            pub extern "C" fn cg_abi_version() -> u32 {
                $crate::ABI_VERSION
            }

            #[unsafe(no_mangle)]
            pub extern "C" fn cg_alloc(len: u32) -> u32 {
                unsafe { $crate::__private::alloc(len) }
            }

            #[unsafe(no_mangle)]
            pub unsafe extern "C" fn cg_free(ptr: u32, len: u32) {
                unsafe { $crate::__private::free(ptr, len) }
            }

            #[unsafe(no_mangle)]
            pub unsafe extern "C" fn cg_init(
                seed: u32,
                seed_len: u32,
                opts: u32,
                opts_len: u32,
            ) -> u64 {
                unsafe {
                    let seed = $crate::__private::input(seed, seed_len);
                    let opts = $crate::__private::input(opts, opts_len);
                    $crate::__private::output(bytes::init::<$game>(seed, opts))
                }
            }

            #[unsafe(no_mangle)]
            pub unsafe extern "C" fn cg_apply(
                state: u32,
                state_len: u32,
                mv: u32,
                mv_len: u32,
                player: u32,
            ) -> u64 {
                unsafe {
                    let state = $crate::__private::input(state, state_len);
                    let mv = $crate::__private::input(mv, mv_len);
                    $crate::__private::output(bytes::apply::<$game>(state, mv, player))
                }
            }

            #[unsafe(no_mangle)]
            pub unsafe extern "C" fn cg_legal_moves(
                state: u32,
                state_len: u32,
                player: u32,
            ) -> u64 {
                unsafe {
                    let state = $crate::__private::input(state, state_len);
                    $crate::__private::output(bytes::legal_moves::<$game>(state, player))
                }
            }

            #[unsafe(no_mangle)]
            pub unsafe extern "C" fn cg_view(state: u32, state_len: u32, player: u32) -> u64 {
                unsafe {
                    let state = $crate::__private::input(state, state_len);
                    $crate::__private::output(bytes::view::<$game>(state, player))
                }
            }

            #[unsafe(no_mangle)]
            pub unsafe extern "C" fn cg_status(state: u32, state_len: u32) -> u64 {
                unsafe {
                    let state = $crate::__private::input(state, state_len);
                    $crate::__private::output(bytes::status::<$game>(state))
                }
            }
        }
    };
}

/// Used by [`export_game!`]; not an API.
#[doc(hidden)]
#[cfg(target_arch = "wasm32")]
pub mod __private {
    use alloc::boxed::Box;
    use alloc::vec;
    use alloc::vec::Vec;

    pub use dlmalloc::GlobalDlmalloc;

    /// A buffer the host fills and later frees with `free`.
    ///
    /// # Safety
    /// Called only through the `cg_alloc` export.
    pub unsafe fn alloc(len: u32) -> u32 {
        let buf: Box<[u8]> = vec![0u8; len as usize].into_boxed_slice();
        Box::into_raw(buf) as *mut u8 as u32
    }

    /// # Safety
    /// `ptr`/`len` must come from `alloc` or from a result returned by `output`.
    pub unsafe fn free(ptr: u32, len: u32) {
        let slice = core::ptr::slice_from_raw_parts_mut(ptr as *mut u8, len as usize);
        drop(unsafe { Box::from_raw(slice) });
    }

    /// # Safety
    /// `ptr`/`len` must name a buffer the host wrote through `alloc`.
    pub unsafe fn input<'a>(ptr: u32, len: u32) -> &'a [u8] {
        if len == 0 {
            return &[];
        }
        unsafe { core::slice::from_raw_parts(ptr as *const u8, len as usize) }
    }

    /// `(ptr << 32) | len`, or 0 when the module failed. The host frees the buffer.
    /// An empty result is still a non-zero value: an empty boxed slice's pointer is
    /// dangling but never null.
    pub fn output(result: Option<Vec<u8>>) -> u64 {
        match result {
            None => 0,
            Some(bytes) => {
                let len = bytes.len() as u64;
                let ptr = Box::into_raw(bytes.into_boxed_slice()) as *mut u8 as u32 as u64;
                (ptr << 32) | len
            }
        }
    }
}

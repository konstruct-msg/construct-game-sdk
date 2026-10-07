//! Messages between a Konstruct game module and its host.
//!
//! The authority is `proto/construct_game_abi.proto`; read it for the exports a module
//! provides and what each message means. This crate is `no_std`: it is compiled into
//! every game module as well as into the host.

#![no_std]

extern crate alloc;

include!(concat!(env!("OUT_DIR"), "/construct.game.abi.v1.rs"));

/// The ABI version this crate describes; a module returns it from `cg_abi_version`.
pub const ABI_VERSION: u32 = 1;

/// Length of the seed passed to `cg_init`.
pub const SEED_LEN: usize = 32;

pub use prost::Message;

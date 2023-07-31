//! Interpreter-facing API to the Yk meta-tracer.

#![cfg_attr(test, feature(test))]
#![feature(lazy_cell)]
#![feature(local_key_cell_methods)]
#![feature(naked_functions)]
#![allow(clippy::type_complexity)]
#![allow(clippy::new_without_default)]

mod deopt;
mod frame;
mod jitstats;
mod location;
pub(crate) mod mt;
pub mod trace;

pub use self::location::Location;
pub use self::mt::{HotThreshold, MT};

#[cfg(feature = "yk_jitstate_debug")]
use std::{env, sync::LazyLock};

#[cfg(feature = "yk_jitstate_debug")]
static JITSTATE_DEBUG: LazyLock<bool> = LazyLock::new(|| env::var("YKD_PRINT_JITSTATE").is_ok());

/// Print select JIT events to stderr for testing/debugging purposes.
#[cfg(feature = "yk_jitstate_debug")]
pub fn print_jit_state(state: &str) {
    if *JITSTATE_DEBUG {
        eprintln!("jit-state: {}", state);
    }
}

#[cfg(feature = "yk_testing")]
mod testing;

//! Trace location: track the state of a program location (counting, tracing, compiled, etc).

use std::{
    convert::TryFrom,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

use crate::mt::HotThreshold;
use parking_lot::Mutex;
use parking_lot_core::{
    park, unpark_one, ParkResult, SpinWait, UnparkResult, UnparkToken, DEFAULT_PARK_TOKEN,
};
use strum::EnumDiscriminants;
use yktrace::{CompiledTrace, ThreadTracer};

/// A `Location` stores state that the meta-tracer needs to identify hot loops and run associated
/// machine code.
///
/// Each position in the end user's program that may be a control point (i.e. the possible start of
/// a trace) must have an associated `Location`. The `Location` does not need to be at a stable
/// address in memory and can be freely moved.
///
/// Program positions that can't be control points don't need an associated `Location`. For
/// interpreters that can't (or don't want) to be as selective, a simple (if moderately wasteful)
/// mechanism is for every bytecode or AST node to have its own `Location` (even for bytecodes or
/// nodes that can't be control points).
#[derive(Debug)]
pub struct Location {
    // A Location is a state machine which operates as follows (where Counting is the start
    // state):
    //
    //              ┌──────────────┐
    //              │              │─────────────┐
    //   reprofile  │   Counting   │             │
    //  ┌──────────▶│              │◀────────────┘
    //  │           └──────────────┘    increment
    //  │             │                 count
    //  │             │ start tracing
    //  │             ▼
    //  │           ┌──────────────┐
    //  |           |              | incomplete  ┌─────────────┐
    //  │           │   Tracing    │────────────▶|  DontTrace  |
    //  |           |              |             └─────────────┘
    //  │           └──────────────┘
    //  │             │ start compiling trace
    //  │             │ in thread
    //  │             ▼
    //  │           ┌──────────────┐
    //  |           |  Compiling   |
    //  │           └──────────────┘
    //  │             │
    //  │             │ trace compiled
    //  │             ▼
    //  │           ┌──────────────┐
    //  └───────────│   Compiled   |
    //              └──────────────┘
    //
    // We hope that a Location soon reaches the Compiled state (aka "the happy state") and stays
    // there.
    //
    // The state machine is encoded in a usize in a not-entirely-simple way, as we don't want to
    // allocate any memory for Locations that do not become hot. The layout is as follows (on a 64
    // bit machine):
    //
    //   bit(s) | 63..3   | 2           | 1         | 0
    //          | payload | IS_COUNTING | IS_PARKED | IS_LOCKED
    //
    // In the Counting state, IS_COUNTING is set to 1, and IS_PARKED and IS_LOCKED are unused and
    // must remain set at 0. The payload representing the count is incremented locklessly. All
    // other states have IS_COUNTING set to 0 and the payload is the address of a boxed
    // HotLocation, access to which is controlled by IS_LOCKED.
    //
    // The possible combinations of the counting and mutex bits are thus as follows:
    //
    //   payload       | IS_COUNTING | IS_PARKED | IS_LOCKED | Notes
    //   --------------+-----------+---------+---------+-----------------------------------
    //   <count>       | 1           | 0         | 0         | Start state
    //                 | 1           | 0         | 1         | Illegal state
    //                 | 1           | 1         | 0         | Illegal state
    //                 | 1           | 1         | 1         | Illegal state
    //   <HotLocation> | 0           | 0         | 0         | Not locked, no-one waiting
    //   <HotLocation> | 0           | 0         | 1         | Locked, no thread(s) waiting
    //   <HotLocation> | 0           | 1         | 0         | Not locked, thread(s) waiting
    //   <HotLocation> | 0           | 1         | 1         | Locked, thread(s) waiting
    //
    // where `<count>` is an integer and `<HotLocation>` is a boxed `HotLocation` enum.
    //
    // The precise semantics of locking and, in particular, parking are subtle: interested readers
    // are directed to https://github.com/Amanieu/parking_lot/blob/master/src/raw_mutex.rs#L33 for
    // a more precise definition.
    state: AtomicUsize,
}

impl Location {
    /// Create a new location.
    pub fn new() -> Self {
        // Locations start in the counting state with a count of 0.
        Self {
            state: AtomicUsize::new(State::new().x),
        }
    }

    /// Return this Location's internal state.
    pub(super) fn load(&self, order: Ordering) -> State {
        State {
            x: self.state.load(order),
        }
    }

    pub(super) fn compare_exchange_weak(
        &self,
        current: State,
        new: State,
        success: Ordering,
        failure: Ordering,
    ) -> Result<State, State> {
        match self
            .state
            .compare_exchange_weak(current.x, new.x, success, failure)
        {
            Ok(x) => Ok(State { x }),
            Err(x) => Err(State { x }),
        }
    }

    /// Locks this `State` with `Acquire` ordering. If the location was in, or moves to, the
    /// Counting state this will return `Err`.
    pub(super) fn lock(&self) -> Result<State, ()> {
        {
            let ls = self.load(Ordering::Relaxed);
            if ls.is_counting() {
                return Err(());
            }
            let new_ls = ls.with_lock().with_unparked();
            if self
                .state
                .compare_exchange_weak(
                    ls.with_unlock().with_unparked().x,
                    new_ls.x,
                    Ordering::Acquire,
                    Ordering::Relaxed,
                )
                .is_ok()
            {
                return Ok(new_ls);
            }
        }

        let mut spinwait = SpinWait::new();
        let mut ls = self.load(Ordering::Relaxed);
        loop {
            if ls.is_counting() {
                return Err(());
            }

            if !ls.is_locked() {
                let new_ls = ls.with_lock();
                match self.state.compare_exchange_weak(
                    ls.x,
                    new_ls.x,
                    Ordering::Acquire,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => return Ok(new_ls),
                    Err(x) => ls = State::from_usize(x),
                }
                continue;
            }

            if !ls.is_parked() {
                if spinwait.spin() {
                    ls = self.load(Ordering::Relaxed);
                    continue;
                } else if let Err(x) = self.state.compare_exchange_weak(
                    ls.x,
                    ls.with_parked().x,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    ls = State::from_usize(x);
                    continue;
                }
            }

            let key = unsafe { ls.hot_location() } as *const _ as usize;
            debug_assert_ne!(key, 0);
            let validate = || {
                let ls = self.load(Ordering::Relaxed);
                ls.is_locked() && ls.is_parked()
            };
            let before_sleep = || {};
            let timed_out = |_, _| unreachable!();
            match unsafe {
                park(
                    key,
                    validate,
                    before_sleep,
                    timed_out,
                    DEFAULT_PARK_TOKEN,
                    None,
                )
            } {
                ParkResult::Unparked(TOKEN_HANDOFF) => return Ok(self.load(Ordering::Relaxed)),
                ParkResult::Invalid | ParkResult::Unparked(_) => (),
                ParkResult::TimedOut => unreachable!(),
            }

            // Loop back and try locking again
            spinwait.reset();
            ls = self.load(Ordering::Relaxed);
        }
    }

    /// Unlocks this `State` with `Release` ordering.
    pub(super) fn unlock(&self) {
        let ls = self.load(Ordering::Relaxed);
        debug_assert!(ls.is_locked());
        debug_assert!(!ls.is_counting());
        if self
            .state
            .compare_exchange(
                ls.with_unparked().x,
                ls.with_unparked().with_unlock().x,
                Ordering::Release,
                Ordering::Relaxed,
            )
            .is_ok()
        {
            return;
        }

        // At this point we know another thread has parked itself.
        let key = unsafe { ls.hot_location() } as *const _ as usize;
        debug_assert_ne!(key, 0);
        let callback = |result: UnparkResult| {
            if result.unparked_threads != 0 && result.be_fair {
                if !result.have_more_threads {
                    debug_assert!(ls.is_locked());
                    self.state.store(ls.with_unparked().x, Ordering::Relaxed);
                }
                return TOKEN_HANDOFF;
            }

            if result.have_more_threads {
                self.state
                    .store(ls.with_unlock().with_parked().x, Ordering::Release);
            } else {
                self.state
                    .store(ls.with_unparked().with_unlock().x, Ordering::Release);
            }
            TOKEN_NORMAL
        };
        unsafe {
            unpark_one(key, callback);
        }
    }

    /// Try obtaining the lock, returning the new `State` if successful.
    pub(super) fn try_lock(&self) -> Result<State, ()> {
        let mut ls = self.load(Ordering::Relaxed);
        loop {
            if ls.is_locked() {
                return Err(());
            }
            let new_ls = ls.with_lock();
            match self.compare_exchange_weak(ls, new_ls, Ordering::Acquire, Ordering::Relaxed) {
                Ok(_) => return Ok(new_ls),
                Err(x) => ls = x,
            }
        }
    }
}

impl Drop for Location {
    fn drop(&mut self) {
        let ls = self.load(Ordering::Relaxed);
        if !ls.is_counting() {
            debug_assert!(!ls.is_locked());
            let hr = unsafe { ls.hot_location() };
            unsafe {
                Box::from_raw(hr);
            }
        }
    }
}

#[cfg(target_pointer_width = "64")]
const STATE_TAG: usize = 0b111; // All of the other tag data must fit in this.
#[cfg(target_pointer_width = "64")]
const STATE_NUM_BITS: usize = 3;

const STATE_IS_LOCKED: usize = 0b001;
const STATE_IS_PARKED: usize = 0b010;
const STATE_IS_COUNTING: usize = 0b100;

const TOKEN_NORMAL: UnparkToken = UnparkToken(0);
const TOKEN_HANDOFF: UnparkToken = UnparkToken(1);

#[derive(PartialEq, Eq, Debug)]
pub(super) struct State {
    x: usize,
}

impl Copy for State {}

impl Clone for State {
    fn clone(&self) -> State {
        *self
    }
}

impl State {
    /// Return a new `State` in the counting phase with a count 0.
    pub(super) fn new() -> Self {
        State {
            x: STATE_IS_COUNTING,
        }
    }

    fn from_usize(x: usize) -> Self {
        State { x }
    }

    pub(super) fn is_locked(&self) -> bool {
        self.x & STATE_IS_LOCKED != 0
    }

    pub(super) fn is_parked(&self) -> bool {
        self.x & STATE_IS_PARKED != 0
    }

    /// Is the Location in the counting or a non-counting state?
    pub(super) fn is_counting(&self) -> bool {
        self.x & STATE_IS_COUNTING != 0
    }

    /// If, and only if, the Location is in the counting state, return the current count.
    pub(super) fn count(&self) -> HotThreshold {
        debug_assert!(self.is_counting());
        debug_assert!(!self.is_locked());
        u32::try_from(self.x >> STATE_NUM_BITS).unwrap()
    }

    /// If this `State` is not counting, return its `HotLocation`. It is undefined behaviour to
    /// call this function if this `State` is in the counting phase and/or if this `State` is not
    /// locked.
    pub(super) unsafe fn hot_location(&self) -> &mut HotLocation {
        debug_assert!(!self.is_counting());
        //debug_assert!(self.is_locked());
        &mut *((self.x & !STATE_TAG) as *mut _)
    }

    /// Return a version of this `State` with the locked bit set.
    pub(super) fn with_lock(&self) -> State {
        State {
            x: self.x | STATE_IS_LOCKED,
        }
    }

    /// Return a version of this `State` with the locked bit unset.
    fn with_unlock(&self) -> State {
        State {
            x: self.x & !STATE_IS_LOCKED,
        }
    }

    /// Return a version of this `State` with the parked bit set.
    fn with_parked(&self) -> State {
        State {
            x: self.x | STATE_IS_PARKED,
        }
    }

    /// Return a version of this `State` with the parked bit unset.
    fn with_unparked(&self) -> State {
        State {
            x: self.x & !STATE_IS_PARKED,
        }
    }

    /// Return a version of this `State` with the count set to `count`. It is undefined behaviour
    /// to call this function if this `State` is not in the counting phase.
    pub(super) fn with_count(&self, count: HotThreshold) -> Self {
        debug_assert!(self.is_counting());
        debug_assert_eq!(count << STATE_NUM_BITS >> STATE_NUM_BITS, count);
        State {
            x: (self.x & STATE_TAG) | (usize::try_from(count).unwrap() << STATE_NUM_BITS),
        }
    }

    /// Set this `State`'s `HotLocation`. It is undefined behaviour for this `State` to already
    /// have a `HotLocation`.
    pub(super) fn with_hotlocation(&self, hl_ptr: *mut HotLocation) -> Self {
        debug_assert!(self.is_counting());
        let hl_ptr = hl_ptr as usize;
        debug_assert_eq!(hl_ptr & !STATE_TAG, hl_ptr);
        State {
            x: (self.x & (STATE_TAG & !STATE_IS_COUNTING)) | hl_ptr,
        }
    }
}

/// An opaque struct used by `MTThreadInner` to help identify if a thread that started a trace is
/// still active.
pub(super) struct ThreadIdInner;

/// A `Location`'s non-counting states.
#[derive(EnumDiscriminants)]
pub(super) enum HotLocation {
    Compiled(Box<CompiledTrace>),
    Compiling(Arc<Mutex<Option<Box<CompiledTrace>>>>),
    DontTrace,
    Tracing(Option<(Arc<ThreadIdInner>, ThreadTracer)>),
}

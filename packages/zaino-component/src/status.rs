//! What a component reports about itself — the read side.

use crate::{Health, Lifecycle};

/// A component's full current state: its management [`Lifecycle`] phase and its
/// [`Health`] condition, reported together and read independently.
///
/// This full state is the source of truth a supervisor acts on. Condensed,
/// app-wide signals (a liveness / readiness bool) are a *projection* taken at
/// the daemon edge, not carried on every component — so they are deliberately
/// absent here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ComponentStatus {
    /// The management phase.
    pub lifecycle: Lifecycle,
    /// The health condition.
    pub health: Health,
}

/// Anything that reports a [`ComponentStatus`].
///
/// Universal to components; the runtime observes it. Cheap and synchronous —
/// reading a status must never await, so a supervisor can sample the whole
/// orchestra without yielding.
pub trait StatusSource {
    /// This component's current state.
    fn status(&self) -> ComponentStatus;
}

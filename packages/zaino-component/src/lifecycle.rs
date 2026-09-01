//! A component's lifecycle.

/// Where a component is in its managed progression — a *phase*.
///
/// Moved only by management ([`Managed`](crate::Managed)): spawn advances it,
/// stop retires it. Distinct from [`Health`](crate::Health), which is a
/// condition the component reaches on its own. A component being `Ready` says
/// nothing about whether it is `Healthy`, and vice versa.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lifecycle {
    /// Starting up; not yet doing its work.
    Spawning,
    /// Running, but still catching up to where it can serve.
    Syncing,
    /// Running and able to serve.
    Ready,
    /// Shutting down.
    Closing,
}

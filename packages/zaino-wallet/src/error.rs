//! Adapter error type.

use zaino_primitives::types::HeightOverflow;
use zaino_service::error::{AddressReadError, BroadcastRejection, Transient};

/// A wallet-adapter failure. Chains the inner per-capability error as its
/// `source`.
#[derive(Debug, thiserror::Error)]
pub enum WalletError {
    /// Could not acquire a coherent snapshot — the one reorg race ADR-0003 admits.
    #[error(transparent)]
    Snapshot(#[from] Transient),
    /// The engine rejected the broadcast (a domain answer, not a backend failure).
    #[error(transparent)]
    Broadcast(#[from] BroadcastRejection),
    /// A caller-supplied height exceeds the protocol's valid range.
    #[error(transparent)]
    InvalidHeight(#[from] HeightOverflow),
    /// The address read failed (not serviceable yet, over budget, or a
    /// backend failure).
    #[error(transparent)]
    AddressRead(#[from] AddressReadError),
}

//! Adapter error type.

use zaino_primitives::types::HeightOverflow;
use zaino_service::error::{
    AddressReadError, BlockReadError, BroadcastRejection, ReadError, Transient, TreestateReadError,
    TxReadError,
};

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
    /// The transaction read failed (status or the pool-decomposed form).
    #[error(transparent)]
    TransactionRead(#[from] TxReadError),
    /// The block read failed (raw bytes or the domain form).
    #[error(transparent)]
    BlockRead(#[from] BlockReadError),
    /// The treestate or subtree-roots read failed.
    #[error(transparent)]
    TreestateRead(#[from] TreestateReadError),
    /// A generic read failed (reported upgrades, or one item of a streamed
    /// read such as `stream_compact`).
    #[error(transparent)]
    Read(#[from] ReadError),
}

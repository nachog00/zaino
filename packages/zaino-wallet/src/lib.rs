//! `zaino-wallet` — the full-wallet library adapter.
//!
//! A stable-DTO facade over the [`FullWalletService`] inner port. A wallet that
//! embeds Zaino (zallet) holds an [`Indexer`] and speaks its DTOs; the inner
//! surface's domain types are converted here, at the adapter, so the inner
//! primitives can evolve without breaking the embedded consumer.
//!
//! The handle is named [`Indexer`] because that is what it is *from the wallet's
//! point of view*: the thing it queries for chain data. It is bound to
//! [`FullWalletService`] alone — not the whole inner surface — so it depends on
//! exactly the capabilities the full-wallet use case needs, and nothing else.
//!
//! This is the driving-adapter half of the hexagon: a public library *is* a
//! driving adapter with its own stable types.
#![forbid(unsafe_code)]

mod dto;
mod error;
mod snapshot;

pub use dto::{WalletTip, WalletTxId};
pub use error::WalletError;
pub use snapshot::WalletSnapshot;

use zaino_service::FullWalletService;

/// The indexer a full wallet library queries, over a [`FullWalletService`] engine.
pub struct Indexer<W: FullWalletService> {
    engine: W,
}

impl<W: FullWalletService> Indexer<W> {
    pub fn new(engine: W) -> Self {
        Self { engine }
    }

    /// Pin the current view, for every read that must stay coherent against
    /// one tip.
    pub async fn snapshot(&self) -> Result<WalletSnapshot<W::Snapshot>, WalletError> {
        let snap = self.engine.snapshot().await?;
        Ok(WalletSnapshot { snap })
    }

    /// Relay a signed transaction; returns its id.
    pub async fn broadcast(&self, raw_tx: Vec<u8>) -> Result<WalletTxId, WalletError> {
        let id = self.engine.broadcast(raw_tx).await?;
        Ok(WalletTxId::from_domain(id))
    }
}

#[cfg(test)]
mod tests {
    use super::{Indexer, WalletTxId};
    use zaino_service::testing::{MockChain, MockIndexerService};

    /// Broadcast routes through the control capability and maps the id out.
    #[tokio::test]
    async fn broadcast_returns_txid_dto() {
        let indexer = Indexer::new(MockIndexerService::new(MockChain::default()));
        let id = indexer.broadcast(vec![1, 2, 3]).await.expect("broadcast");
        assert_eq!(id, WalletTxId([0u8; 32]));
    }
}

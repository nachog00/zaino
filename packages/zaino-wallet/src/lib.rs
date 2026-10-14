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

pub use dto::{WalletMempoolEvent, WalletReportedUpgrade, WalletTip, WalletTipEvent, WalletTxId};
pub use error::WalletError;
pub use snapshot::WalletSnapshot;

use futures::stream::{BoxStream, StreamExt};
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

    /// The validator's reported network-upgrade schedule.
    pub async fn reported_upgrades(&self) -> Result<Vec<WalletReportedUpgrade>, WalletError> {
        let upgrades = self.engine.reported_upgrades().await?;
        Ok(upgrades
            .into_iter()
            .map(WalletReportedUpgrade::from_domain)
            .collect())
    }

    /// The current tip, then every change after it.
    pub fn subscribe_tip(&self) -> BoxStream<'_, WalletTipEvent> {
        self.engine
            .subscribe_tip()
            .map(WalletTipEvent::from_domain)
            .boxed()
    }

    /// Unconfirmed transactions, tip-tagged, independent of chain-tip changes.
    pub fn subscribe_mempool(&self) -> BoxStream<'_, WalletMempoolEvent> {
        self.engine
            .subscribe_mempool()
            .map(WalletMempoolEvent::from_domain)
            .boxed()
    }
}

#[cfg(test)]
mod tests {
    use super::{Indexer, WalletTxId};
    use futures::StreamExt;
    use zaino_primitives::types::{BlockHash, BlockRef, Height, TransactionId};
    use zaino_service::testing::{MockChain, MockIndexerService};
    use zaino_service::MempoolTx;

    /// Broadcast routes through the control capability and maps the id out.
    #[tokio::test]
    async fn broadcast_returns_txid_dto() {
        let indexer = Indexer::new(MockIndexerService::new(MockChain::default()));
        let id = indexer.broadcast(vec![1, 2, 3]).await.expect("broadcast");
        assert_eq!(id, WalletTxId([0u8; 32]));
    }

    /// The mock reports no reported upgrades; proves the adapter delegates and
    /// maps an empty answer correctly.
    #[tokio::test]
    async fn reported_upgrades_maps_to_dto() {
        let indexer = Indexer::new(MockIndexerService::new(MockChain::default()));
        let upgrades = indexer
            .reported_upgrades()
            .await
            .expect("reported upgrades");
        assert!(upgrades.is_empty());
    }

    /// The current tip is the first event the stream yields.
    #[tokio::test]
    async fn subscribe_tip_yields_the_current_tip() {
        let tip = BlockRef {
            height: Height::try_from(3).expect("valid height"),
            hash: BlockHash::from([2u8; 32]),
        };
        let indexer = Indexer::new(MockIndexerService::new(MockChain {
            tip: Some(tip),
            ..Default::default()
        }));

        let event = indexer
            .subscribe_tip()
            .next()
            .await
            .expect("at least the current tip");

        assert_eq!(
            event,
            super::WalletTipEvent {
                tip: super::WalletTip {
                    height: 3,
                    hash: [2u8; 32],
                },
            }
        );
    }

    /// A scripted mempool entry maps to the stable DTO.
    #[tokio::test]
    async fn subscribe_mempool_yields_scripted_entries() {
        let validated_against = BlockRef {
            height: Height::try_from(1).expect("valid height"),
            hash: BlockHash::from([1u8; 32]),
        };
        let indexer = Indexer::new(MockIndexerService::new(MockChain {
            mempool: vec![MempoolTx {
                txid: TransactionId::from([5u8; 32]),
                validated_against,
            }],
            ..Default::default()
        }));

        let event = indexer
            .subscribe_mempool()
            .next()
            .await
            .expect("the scripted entry");

        assert_eq!(
            event,
            super::WalletMempoolEvent {
                txid: WalletTxId([5u8; 32]),
                validated_against: super::WalletTip {
                    height: 1,
                    hash: [1u8; 32],
                },
            }
        );
    }
}

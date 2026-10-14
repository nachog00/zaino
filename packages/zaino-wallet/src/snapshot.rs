//! The pinned view a full wallet reads through.
//!
//! Mirrors zallet's own split: an engine handle (`Indexer`) that hands out a
//! pinned view, and the view itself (`WalletSnapshot`) carrying every read
//! that must stay coherent against one tip. A wallet scanning a batch of
//! blocks, then checking an address's balance, wants both answered against
//! the same chain position — re-pinning per call could see the tip move
//! between them.

use zaino_service::{ChainSegment, FullWalletReads};

use crate::dto::WalletTip;

/// A pinned view into the chain, over every read a full wallet needs.
pub struct WalletSnapshot<S> {
    pub(crate) snap: S,
}

impl<S: ChainSegment + FullWalletReads> WalletSnapshot<S> {
    /// The tip this view is coherent against. `None` when the chain has no
    /// tip yet.
    pub fn tip(&self) -> Option<WalletTip> {
        self.snap.pinned_tip().map(WalletTip::from_domain)
    }
}

#[cfg(test)]
mod tests {
    use crate::Indexer;
    use zaino_primitives::types::{BlockHash, BlockRef, Height};
    use zaino_service::testing::{MockChain, MockIndexerService};

    /// A pinned snapshot's tip maps the domain `BlockRef` to the stable DTO.
    #[tokio::test]
    async fn snapshot_tip_maps_to_dto() {
        let tip = BlockRef {
            height: Height::try_from(42).expect("valid height"),
            hash: BlockHash::from([7u8; 32]),
        };
        let indexer = Indexer::new(MockIndexerService::new(MockChain {
            tip: Some(tip),
            ..Default::default()
        }));

        let snapshot = indexer.snapshot().await.expect("snapshot acquired");

        assert_eq!(
            snapshot.tip().expect("some tip"),
            crate::WalletTip {
                height: 42,
                hash: [7u8; 32],
            }
        );
    }

    /// An empty chain has no pinned tip.
    #[tokio::test]
    async fn snapshot_tip_is_none_on_an_empty_chain() {
        let indexer = Indexer::new(MockIndexerService::new(MockChain::default()));

        let snapshot = indexer.snapshot().await.expect("snapshot acquired");

        assert!(snapshot.tip().is_none());
    }
}

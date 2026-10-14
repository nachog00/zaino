//! The pinned view a full wallet reads through.
//!
//! Mirrors zallet's own split: an engine handle (`Indexer`) that hands out a
//! pinned view, and the view itself (`WalletSnapshot`) carrying every read
//! that must stay coherent against one tip. A wallet scanning a batch of
//! blocks, then checking an address's balance, wants both answered against
//! the same chain position — re-pinning per call could see the tip move
//! between them.

use zaino_primitives::types::{Height, HeightOverflow, HeightRange, TransparentAddress};
use zaino_service::{ChainSegment, FullWalletReads, ReadBudget};

use crate::dto::{
    WalletAddressBalance, WalletAddressDelta, WalletLocatedTxid, WalletTip, WalletUtxo,
};
use crate::WalletError;

/// A pinned view into the chain, over every read a full wallet needs.
pub struct WalletSnapshot<S> {
    pub(crate) snap: S,
}

/// `[start, end]` as a validated [`HeightRange`] — the door every
/// range-taking method passes a caller's raw heights through.
fn height_range(start: u32, end: u32) -> Result<HeightRange, HeightOverflow> {
    Ok(HeightRange {
        start: Height::try_from(start)?,
        end: Height::try_from(end)?,
    })
}

impl<S: ChainSegment + FullWalletReads> WalletSnapshot<S> {
    /// The tip this view is coherent against. `None` when the chain has no
    /// tip yet.
    pub fn tip(&self) -> Option<WalletTip> {
        self.snap.pinned_tip().map(WalletTip::from_domain)
    }

    /// A transparent address's balance over `[start, end]`.
    pub async fn address_balance(
        &self,
        address: &str,
        start: u32,
        end: u32,
    ) -> Result<WalletAddressBalance, WalletError> {
        let range = height_range(start, end)?;
        let balance = self
            .snap
            .balance(
                &TransparentAddress::new(address.to_owned()),
                range,
                &mut ReadBudget::for_request(),
            )
            .await?;
        Ok(WalletAddressBalance::from_domain(balance))
    }

    /// A transparent address's unspent outputs.
    pub async fn address_utxos(&self, address: &str) -> Result<Vec<WalletUtxo>, WalletError> {
        let utxos = self
            .snap
            .unspent_outpoints(
                &TransparentAddress::new(address.to_owned()),
                &mut ReadBudget::for_request(),
            )
            .await?;
        Ok(utxos.into_iter().map(WalletUtxo::from_domain).collect())
    }

    /// A transparent address's balance changes over `[start, end]`.
    pub async fn address_deltas(
        &self,
        address: &str,
        start: u32,
        end: u32,
    ) -> Result<Vec<WalletAddressDelta>, WalletError> {
        let range = height_range(start, end)?;
        let deltas = self
            .snap
            .deltas(
                &TransparentAddress::new(address.to_owned()),
                range,
                &mut ReadBudget::for_request(),
            )
            .await?;
        Ok(deltas
            .into_iter()
            .map(WalletAddressDelta::from_domain)
            .collect())
    }

    /// Every transaction touching a transparent address over `[start, end]`.
    pub async fn address_tx_ids(
        &self,
        address: &str,
        start: u32,
        end: u32,
    ) -> Result<Vec<WalletLocatedTxid>, WalletError> {
        let range = height_range(start, end)?;
        let located = self
            .snap
            .tx_ids(
                &TransparentAddress::new(address.to_owned()),
                range,
                &mut ReadBudget::for_request(),
            )
            .await?;
        Ok(located
            .into_iter()
            .map(WalletLocatedTxid::from_domain)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use crate::Indexer;
    use zaino_primitives::types::{
        AddressBalance, BlockHash, BlockRef, Height, Zatoshis, ZatoshisFlowSum,
    };
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

    /// A scripted address balance maps to the stable DTO, zatoshis converted
    /// to their plain integer widths.
    #[tokio::test]
    async fn address_balance_maps_to_dto() {
        let indexer = Indexer::new(MockIndexerService::new(MockChain {
            balances: vec![(
                "t1exampleaddress".to_owned(),
                AddressBalance {
                    balance: Zatoshis::new(1_000u64).expect("valid amount"),
                    received: ZatoshisFlowSum::from_summed(5_000u64),
                },
            )],
            ..Default::default()
        }));
        let snapshot = indexer.snapshot().await.expect("snapshot acquired");

        let balance = snapshot
            .address_balance("t1exampleaddress", 0, 10)
            .await
            .expect("balance");

        assert_eq!(
            balance,
            crate::dto::WalletAddressBalance {
                balance: 1_000,
                received: 5_000,
            }
        );
    }

    /// A height past the protocol maximum is a typed error, not a panic.
    #[tokio::test]
    async fn address_balance_rejects_an_invalid_height() {
        let indexer = Indexer::new(MockIndexerService::new(MockChain::default()));
        let snapshot = indexer.snapshot().await.expect("snapshot acquired");

        let err = snapshot
            .address_balance("t1exampleaddress", 0, u32::MAX)
            .await
            .expect_err("a height beyond the protocol maximum must be rejected");

        assert!(matches!(err, crate::WalletError::InvalidHeight(_)));
    }

    /// A scripted UTXO maps to the stable DTO.
    #[tokio::test]
    async fn address_utxos_map_to_dto() {
        use zaino_primitives::types::{
            Height, Script, TransactionId, TransparentAddress, Utxo, Zatoshis,
        };

        let indexer = Indexer::new(MockIndexerService::new(MockChain {
            utxos: vec![Utxo {
                address: TransparentAddress::new("t1exampleaddress".to_owned()),
                txid: TransactionId::from([3u8; 32]),
                output_index: 1,
                script: Script::new(vec![0xAA]),
                satoshis: Zatoshis::new(500).expect("valid amount"),
                height: Height::try_from(10).expect("valid height"),
            }],
            ..Default::default()
        }));
        let snapshot = indexer.snapshot().await.expect("snapshot acquired");

        let utxos = snapshot
            .address_utxos("t1exampleaddress")
            .await
            .expect("utxos");

        assert_eq!(
            utxos,
            vec![crate::dto::WalletUtxo {
                address: "t1exampleaddress".to_owned(),
                txid: [3u8; 32],
                output_index: 1,
                script: vec![0xAA],
                satoshis: 500,
                height: 10,
            }]
        );
    }

    /// A scripted delta, inside the requested range, maps to the stable DTO.
    #[tokio::test]
    async fn address_deltas_map_to_dto() {
        use zaino_primitives::types::{
            AddressDelta, Height, SignedZatoshis, TransactionId, TransparentAddress,
        };

        let indexer = Indexer::new(MockIndexerService::new(MockChain {
            deltas: vec![AddressDelta {
                satoshis: SignedZatoshis::try_new(-200i64).expect("valid amount"),
                txid: TransactionId::from([4u8; 32]),
                index: 0,
                height: Height::try_from(5).expect("valid height"),
                address: TransparentAddress::new("t1exampleaddress".to_owned()),
                block_index: Some(2),
            }],
            ..Default::default()
        }));
        let snapshot = indexer.snapshot().await.expect("snapshot acquired");

        let deltas = snapshot
            .address_deltas("t1exampleaddress", 0, 10)
            .await
            .expect("deltas");

        assert_eq!(
            deltas,
            vec![crate::dto::WalletAddressDelta {
                satoshis: -200,
                txid: [4u8; 32],
                index: 0,
                height: 5,
                address: "t1exampleaddress".to_owned(),
                block_index: Some(2),
            }]
        );
    }

    /// Scripted txids map to the stable DTO, in the mock's ascending-height order.
    #[tokio::test]
    async fn address_tx_ids_map_to_dto() {
        use zaino_primitives::types::TransactionId;

        let indexer = Indexer::new(MockIndexerService::new(MockChain {
            txids: vec![TransactionId::from([1u8; 32])],
            ..Default::default()
        }));
        let snapshot = indexer.snapshot().await.expect("snapshot acquired");

        let located = snapshot
            .address_tx_ids("t1exampleaddress", 0, 10)
            .await
            .expect("tx ids");

        assert_eq!(
            located,
            vec![crate::dto::WalletLocatedTxid {
                height: Some(0),
                block_index: None,
                txid: [1u8; 32],
            }]
        );
    }
}

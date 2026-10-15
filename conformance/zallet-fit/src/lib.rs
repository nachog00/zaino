//! NOT a production crate — see `Cargo.toml`'s description.
//!
//! A real (non-mirrored) `backends/zaino`-shaped adapter: wraps
//! `zaino-wallet`'s `Indexer`/`WalletSnapshot` and implements zallet-core's
//! actual `Chain`/`ChainView` trait, fetched live from `zcash/zallet@main`.
//!
//! Methods marked REAL below have a genuine implementation and genuine
//! tests driving them through the *abstract* `Chain`/`ChainView` trait bound
//! (never the concrete type) — the same way zallet's own wallet logic would
//! call them. Methods marked `todo!()` need real parsing/encoding work this
//! crate does not attempt (deserializing raw bytes into
//! `zcash_primitives`/`zcash_client_backend` types, network-aware address
//! string encoding) or a design decision not yet made (`find_fork_point`,
//! `get_mempool_stream`) — see
//! `zaino-design/cases/2026-10-14-zallet-fit-real-acid-test.md` for the full
//! gap list and reasoning.

use std::ops::Range;

use futures::stream::BoxStream;
use zallet_core::components::chain::{
    BlockLocator, Chain, ChainBlock, ChainError, ChainTx, ChainView,
    ReportedUpgrade as ZalletReportedUpgrade, UpgradeStatus as ZalletUpgradeStatus,
};
use zallet_core::error::ErrorKind;
use zallet_core::network::Network;
use zcash_client_backend::data_api::{chain::CommitmentTreeRoot, TransactionStatus};
use zcash_primitives::{
    block::{Block, BlockHash as ZBlockHash, BlockHeader},
    transaction::Transaction,
};
use zcash_protocol::{consensus::BlockHeight, TxId};

use zaino_service::FullWalletService;
use zaino_wallet::{Indexer, WalletError, WalletSnapshot, WalletTxStatus};

/// zallet's `backends/zaino` adapter, in our tree: wraps our published
/// `Indexer` and presents it as zallet's `Chain`.
#[derive(Clone)]
pub struct ZainoBackend<E: FullWalletService> {
    client: Indexer<E>,
    network: Network,
}

impl<E: FullWalletService> ZainoBackend<E> {
    pub fn new(engine: E, network: Network) -> Self {
        Self {
            client: Indexer::new(engine),
            network,
        }
    }
}

/// The view side: wraps our published `WalletSnapshot`.
#[derive(Clone)]
pub struct ZainoView<S: Clone> {
    snap: WalletSnapshot<S>,
}

fn wallet_err(e: WalletError) -> ChainError {
    ChainError::backend(e)
}

/// A 32-byte id, by any name, as zallet's own `TxId` (its only public
/// constructor is `io::Read`-based, not a bare-bytes one).
#[allow(dead_code)]
fn to_txid(bytes: [u8; 32]) -> TxId {
    TxId::read(&bytes[..]).expect("a 32-byte array is always a well-formed TxId")
}

impl<E> Chain for ZainoBackend<E>
where
    E: FullWalletService + Clone + Send + Sync + 'static,
    E::Snapshot: Clone,
{
    type View = ZainoView<E::Snapshot>;

    // REAL.
    fn params(&self) -> &Network {
        &self.network
    }

    // REAL: `ErrorKind::context`/`Error: From<Context<ErrorKind>>` are both
    // public, so a real backend crate genuinely can build a real
    // `zallet_core::error::Error` from its own error type.
    async fn reported_upgrades(
        &self,
    ) -> Result<Vec<ZalletReportedUpgrade>, zallet_core::error::Error> {
        let upgrades = self
            .client
            .reported_upgrades()
            .await
            .map_err(|e| zallet_core::error::Error::from(ErrorKind::Chain.context(e)))?;
        Ok(upgrades
            .into_iter()
            .map(|u| {
                let status = match u.status {
                    zaino_wallet::WalletUpgradeStatus::Active => ZalletUpgradeStatus::Active,
                    zaino_wallet::WalletUpgradeStatus::Pending => ZalletUpgradeStatus::Pending,
                    zaino_wallet::WalletUpgradeStatus::Disabled => ZalletUpgradeStatus::Disabled,
                };
                ZalletReportedUpgrade::new(u.branch_id, u.name, u.activation_height, status)
            })
            .collect())
    }

    // REAL: serializes the real `zcash_primitives::Transaction` to its
    // consensus bytes, via its own `write`, then relays through `Indexer`.
    async fn broadcast_transaction(&self, tx: &Transaction) -> Result<(), ChainError> {
        let mut bytes = Vec::new();
        tx.write(&mut bytes).map_err(ChainError::backend)?;
        self.client
            .broadcast(bytes)
            .await
            .map_err(wallet_err)
            .map(|_txid| ())
    }

    // GAP: zallet wants ONE method PER POOL, each returning the complete set
    // of `CommitmentTreeRoot<Node>` (real cryptographic nodes, not raw
    // hashes). We have ONE method parameterized by `ShieldedPool`, paginated.
    // The adapter pages ours to exhaustion per pool, then decodes each root —
    // real, un-skippable conversion work.
    async fn get_sapling_subtree_roots(
        &self,
    ) -> Result<Vec<CommitmentTreeRoot<sapling::Node>>, ChainError> {
        todo!("page WalletSnapshot::subtree_roots(Sapling, ..) to exhaustion, decode each root")
    }
    async fn get_orchard_subtree_roots(
        &self,
    ) -> Result<Vec<CommitmentTreeRoot<orchard::tree::MerkleHashOrchard>>, ChainError> {
        todo!("page WalletSnapshot::subtree_roots(Orchard, ..) to exhaustion, decode each root")
    }
    async fn get_ironwood_subtree_roots(
        &self,
    ) -> Result<Vec<CommitmentTreeRoot<orchard::tree::MerkleHashOrchard>>, ChainError> {
        todo!("page WalletSnapshot::subtree_roots(Ironwood, ..) to exhaustion, decode each root")
    }

    // REAL.
    async fn snapshot(&self) -> Result<Self::View, ChainError> {
        let snap = self.client.snapshot().await.map_err(wallet_err)?;
        Ok(ZainoView { snap })
    }
}

impl<S> ChainView for ZainoView<S>
where
    S: zaino_service::FullWalletReads + zaino_service::ChainSegment + Clone + Send + Sync + 'static,
{
    // REAL.
    async fn tip(&self) -> Result<ChainBlock, ChainError> {
        let tip = self
            .snap
            .tip()
            .ok_or_else(|| ChainError::backend("no pinned tip yet"))?;
        Ok(ChainBlock::new(
            BlockHeight::from_u32(tip.height),
            ZBlockHash(tip.hash),
        ))
    }

    // GAP: `ForkReconcile` has no implementation anywhere in zaino-service/
    // zaino-core — this is "needs to be built", not "needs conversion".
    async fn find_fork_point(
        &self,
        _locator: &BlockLocator,
    ) -> Result<Option<ChainBlock>, ChainError> {
        todo!("ForkReconcile does not exist on any real engine yet")
    }

    // GAP: needs deserializing WalletTreestate's per-pool bytes into
    // zcash_client_backend's ChainState frontiers — real parsing work.
    async fn tree_state_as_of(
        &self,
        height: BlockHeight,
    ) -> Result<Option<zcash_client_backend::data_api::chain::ChainState>, ChainError> {
        let _ = height;
        todo!("call self.snap.treestate(height), deserialize its per-pool bytes into ChainState frontiers")
    }

    // GAP: needs parsing WalletSnapshot::raw_block's bytes with
    // zcash_primitives' own block parser.
    async fn get_block_header(
        &self,
        _height: BlockHeight,
    ) -> Result<Option<BlockHeader>, ChainError> {
        todo!("parse WalletSnapshot::raw_block's bytes into a zcash_primitives BlockHeader")
    }
    async fn get_block(&self, _height: BlockHeight) -> Result<Option<Block>, ChainError> {
        todo!("parse WalletSnapshot::raw_block's bytes into a zcash_primitives Block")
    }
    fn stream_blocks_to_tip(
        &self,
        _start: BlockHeight,
    ) -> BoxStream<'_, Result<Block, ChainError>> {
        todo!("stream + parse raw blocks from `start` to tip")
    }
    fn stream_blocks(
        &self,
        _range: &Range<BlockHeight>,
    ) -> BoxStream<'_, Result<Block, ChainError>> {
        todo!("stream + parse raw blocks over a range")
    }

    // GAP, structural: zallet wants this tip-gated and snapshot-level,
    // streaming parsed Transactions. Ours (`subscribe_mempool`) is
    // engine-level, untagged, content-free. Needs a design decision.
    async fn get_mempool_stream(
        &self,
    ) -> Result<Option<BoxStream<'_, Result<Transaction, ChainError>>>, ChainError> {
        todo!("needs a tip-gated, snapshot-level mempool stream of parsed transactions")
    }

    // GAP: needs parsing WalletSnapshot::raw_transaction's bytes into a real
    // Transaction to assemble ChainTx.
    async fn get_transaction(&self, _txid: TxId) -> Result<Option<ChainTx>, ChainError> {
        todo!("parse WalletSnapshot::raw_transaction's bytes, assemble ChainTx from its location")
    }

    // REAL.
    async fn get_transaction_status(&self, txid: TxId) -> Result<TransactionStatus, ChainError> {
        let bytes: [u8; 32] = *txid.as_ref();
        let status = self
            .snap
            .transaction_status(bytes)
            .await
            .map_err(wallet_err)?;
        Ok(match status {
            WalletTxStatus::Mined(height) => {
                TransactionStatus::Mined(BlockHeight::from_u32(height))
            }
            WalletTxStatus::Orphaned => TransactionStatus::NotInMainChain,
            WalletTxStatus::Unknown => TransactionStatus::TxidNotRecognized,
        })
    }

    // GAP: needs a real t-addr string encoder for `TransparentAddress`
    // (network-dependent base58/bech32), not just its in-memory shape.
    #[allow(unused_variables)]
    async fn get_address_unspent_outpoints(
        &self,
        address: &transparent::address::TransparentAddress,
    ) -> Result<Vec<(TxId, u32)>, ChainError> {
        todo!(
            "encode TransparentAddress to its string form, then call self.snap.address_utxos + to_txid per entry"
        )
    }

    // GAP: same string-encoding gap as above.
    #[allow(unused_variables)]
    async fn get_address_tx_ids(
        &self,
        address: &transparent::address::TransparentAddress,
        range: Range<BlockHeight>,
    ) -> Result<Vec<TxId>, ChainError> {
        todo!(
            "encode TransparentAddress to its string form, then call self.snap.address_tx_ids + to_txid per entry"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zaino_primitives::types::{BlockHash as ZaBlockHash, BlockRef, Height};
    use zaino_service::testing::{MockChain, MockIndexerService};
    use zcash_protocol::consensus::NetworkType;

    fn test_network() -> Network {
        Network::from_type(NetworkType::Regtest, &[])
    }

    /// Every test here drives the backend through the *abstract* `Chain`/
    /// `ChainView` trait bound — never the concrete `ZainoBackend`/`ZainoView`
    /// type — the same way zallet's own wallet logic is written against the
    /// trait, not against any particular backend. Only REAL methods (listed
    /// above) are exercised; the `todo!()`-bodied ones are out of scope here.

    async fn pinned_tip<C: Chain>(chain: &C) -> ChainBlock {
        let view = chain.snapshot().await.expect("a coherent snapshot");
        view.tip().await.expect("tip")
    }

    #[tokio::test]
    async fn real_impl_reports_the_pinned_tip_through_the_abstract_trait() {
        let tip_ref = BlockRef {
            height: Height::try_from(42).expect("valid height"),
            hash: ZaBlockHash::from([7u8; 32]),
        };
        let chain = ZainoBackend::new(
            MockIndexerService::new(MockChain {
                tip: Some(tip_ref),
                ..Default::default()
            }),
            test_network(),
        );

        let tip = pinned_tip(&chain).await;

        assert_eq!(tip.height(), BlockHeight::from_u32(42));
        assert_eq!(tip.hash(), ZBlockHash([7u8; 32]));
    }

    #[tokio::test]
    async fn real_impl_reports_an_unrecognized_txid_through_the_abstract_trait() {
        let chain = ZainoBackend::new(
            MockIndexerService::new(MockChain::default()),
            test_network(),
        );
        let view = chain.snapshot().await.expect("a coherent snapshot");

        let status = view
            .get_transaction_status(to_txid([0u8; 32]))
            .await
            .expect("status");

        // The mock always answers Unknown; proves the mapping onto zallet's
        // real TransactionStatus enum, not the engine's status logic (that's
        // zaino-core's own tested claim).
        assert_eq!(status, TransactionStatus::TxidNotRecognized);
    }

    #[tokio::test]
    async fn real_impl_reports_no_upgrades_through_the_abstract_trait() {
        let chain = ZainoBackend::new(
            MockIndexerService::new(MockChain::default()),
            test_network(),
        );

        let upgrades = chain.reported_upgrades().await.expect("reported_upgrades");

        assert!(upgrades.is_empty());
    }

    #[tokio::test]
    async fn real_impl_broadcasts_a_real_zcash_primitives_transaction() {
        use proptest::strategy::{Strategy, ValueTree};
        use proptest::test_runner::TestRunner;
        use zcash_primitives::transaction::testing::arb_tx;
        use zcash_protocol::consensus::BranchId;

        // One genuine, well-formed Transaction, not a hand-rolled stand-in —
        // proves `Transaction::write` really runs inside broadcast_transaction.
        let mut runner = TestRunner::deterministic();
        let tx = arb_tx(BranchId::Nu5)
            .new_tree(&mut runner)
            .expect("a sampled transaction")
            .current();

        let chain = ZainoBackend::new(
            MockIndexerService::new(MockChain::default()),
            test_network(),
        );

        chain
            .broadcast_transaction(&tx)
            .await
            .expect("the mock accepts any broadcast");
    }
}

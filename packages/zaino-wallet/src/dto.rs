//! Stable wallet-facing DTOs.
//!
//! The inner surface's domain types are converted to these here, at the adapter,
//! so the inner primitives can evolve without breaking the embedded wallet.
//! Conversions are named for direction: `from_domain` for a read coming out,
//! `into_domain` for a caller-supplied selector going in.

mod compact_block;

pub use compact_block::{
    WalletChainMetadata, WalletCompactBlock, WalletCompactTx, WalletOrchardAction,
    WalletSaplingOutput, WalletTransparentInput, WalletTransparentOutput,
};

use zaino_primitives::types::{
    AddressBalance, AddressDelta, BlockRef, BlockSelector, Height, HeightOverflow, PoolTreestate,
    RawTransaction, ShieldedPool, SubtreeRoot, TransactionId, TransactionLocation, Treestate, Utxo,
};
use zaino_service::{LocatedTxid, MempoolTx, ReportedUpgrade, TipEvent, TxStatus, UpgradeStatus};

/// The tip a pinned view is coherent as of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WalletTip {
    pub height: u32,
    pub hash: [u8; 32],
}

impl WalletTip {
    pub(crate) fn from_domain(id: BlockRef) -> Self {
        Self {
            height: id.height.into(),
            hash: id.hash.into(),
        }
    }
}

/// A transaction identifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WalletTxId(pub [u8; 32]);

impl WalletTxId {
    pub(crate) fn from_domain(id: TransactionId) -> Self {
        Self(id.into())
    }
}

/// A transparent address's balance: what it currently holds, and what it has
/// ever received.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WalletAddressBalance {
    /// Total value currently held, in zatoshis.
    pub balance: u64,
    /// Lifetime gross receipts, in zatoshis. A flow total, not bounded by the
    /// money supply — see [`zaino_primitives::types::ZatoshisFlowSum`].
    pub received: u128,
}

impl WalletAddressBalance {
    pub(crate) fn from_domain(balance: AddressBalance) -> Self {
        Self {
            balance: balance.balance.into(),
            received: balance.received.into(),
        }
    }
}

/// An unspent transparent output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WalletUtxo {
    /// The transparent address holding this output, as its string form.
    pub address: String,
    /// The transaction containing this output.
    pub txid: [u8; 32],
    /// Output index within the transaction.
    pub output_index: u32,
    /// The output's locking script.
    pub script: Vec<u8>,
    /// Value in zatoshis.
    pub satoshis: u64,
    /// Block height where this output was created.
    pub height: u32,
}

impl WalletUtxo {
    pub(crate) fn from_domain(utxo: Utxo) -> Self {
        Self {
            address: utxo.address.to_string(),
            txid: utxo.txid.into(),
            output_index: utxo.output_index,
            script: utxo.script.into(),
            satoshis: utxo.satoshis.into(),
            height: utxo.height.into(),
        }
    }
}

/// A single balance change for a transparent address.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WalletAddressDelta {
    /// Change in zatoshis (negative for spends, positive for receives).
    pub satoshis: i64,
    /// The transaction that caused this delta.
    pub txid: [u8; 32],
    /// Input or output index within the transaction.
    pub index: u32,
    /// Block height where this delta occurred.
    pub height: u32,
    /// The transparent address affected, as its string form.
    pub address: String,
    /// Zero-based position of the transaction within its containing block,
    /// when the source can supply it.
    pub block_index: Option<u32>,
}

impl WalletAddressDelta {
    pub(crate) fn from_domain(delta: AddressDelta) -> Self {
        Self {
            satoshis: delta.satoshis.into(),
            txid: delta.txid.into(),
            index: delta.index,
            height: delta.height.into(),
            address: delta.address.to_string(),
            block_index: delta.block_index,
        }
    }
}

/// One transaction touching an address, ordered `(height, position, txid)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WalletLocatedTxid {
    /// The height it was mined at, when known (absent over passthrough).
    pub height: Option<u32>,
    /// Its position within that block, when known.
    pub block_index: Option<u32>,
    /// The transaction id.
    pub txid: [u8; 32],
}

impl WalletLocatedTxid {
    pub(crate) fn from_domain(located: LocatedTxid) -> Self {
        let (height, block_index, txid) = located;
        Self {
            height: height.map(Into::into),
            block_index,
            txid: txid.into(),
        }
    }
}

/// Where a transaction lives in the chain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WalletTransactionLocation {
    /// On the best chain at this height.
    BestChain(u32),
    /// On a non-best chain branch (orphaned).
    NonBestChain,
    /// In the mempool (not yet mined).
    Mempool,
}

impl WalletTransactionLocation {
    fn from_domain(location: TransactionLocation) -> Self {
        match location {
            TransactionLocation::BestChain(height) => Self::BestChain(height.into()),
            TransactionLocation::NonBestChain => Self::NonBestChain,
            TransactionLocation::Mempool => Self::Mempool,
        }
    }
}

/// A transaction as raw serialized bytes, plus where it lives. A wallet
/// parses these bytes itself — no decoded transaction type crosses this
/// boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WalletRawTransaction {
    /// The serialized transaction bytes.
    pub data: Vec<u8>,
    /// Where the transaction was found.
    pub location: WalletTransactionLocation,
}

impl WalletRawTransaction {
    pub(crate) fn from_domain(raw: RawTransaction) -> Self {
        Self {
            data: raw.data,
            location: WalletTransactionLocation::from_domain(raw.location),
        }
    }
}

/// Whether a transaction the wallet sent has confirmed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WalletTxStatus {
    /// Mined at this height on the best chain.
    Mined(u32),
    /// Mined, but on a branch that is no longer the best chain.
    Orphaned,
    /// Neither mined nor known to be orphaned (unseen, or still in the
    /// mempool).
    Unknown,
}

impl WalletTxStatus {
    pub(crate) fn from_domain(status: TxStatus) -> Self {
        match status {
            TxStatus::Mined(height) => Self::Mined(height.into()),
            TxStatus::Orphaned => Self::Orphaned,
            TxStatus::Unknown => Self::Unknown,
        }
    }
}

/// How the caller names the block it wants: by height on the best chain, or
/// by hash on any chain the server retains.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WalletBlockSelector {
    /// The block at this height on the best chain.
    Height(u32),
    /// The block with this hash.
    Hash([u8; 32]),
}

impl WalletBlockSelector {
    pub(crate) fn into_domain(self) -> Result<BlockSelector, HeightOverflow> {
        Ok(match self {
            Self::Height(height) => BlockSelector::Height(Height::try_from(height)?),
            Self::Hash(hash) => BlockSelector::Hash(hash.into()),
        })
    }
}

/// Which shielded pool a treestate or subtree-roots query targets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WalletShieldedPool {
    /// Sapling shielded pool.
    Sapling,
    /// Orchard shielded pool.
    Orchard,
    /// Ironwood shielded pool (NU6.3).
    Ironwood,
}

impl WalletShieldedPool {
    pub(crate) fn into_domain(self) -> ShieldedPool {
        match self {
            Self::Sapling => ShieldedPool::Sapling,
            Self::Orchard => ShieldedPool::Orchard,
            Self::Ironwood => ShieldedPool::Ironwood,
        }
    }
}

/// One pool's commitment tree at a block, as the opaque bytes the Zcash
/// protocol serializes. A wallet interprets these itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WalletPoolTreestate {
    /// Root of the pool's note commitment tree after this block, when the
    /// source reports one.
    pub final_root: Option<[u8; 32]>,
    /// The pool's serialized note commitment tree.
    pub final_state: Vec<u8>,
}

impl WalletPoolTreestate {
    fn from_domain(pool: PoolTreestate) -> Self {
        Self {
            final_root: pool.final_root.map(Into::into),
            final_state: pool.final_state,
        }
    }
}

/// The commitment trees as of a block, and which block that is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WalletTreestate {
    /// The block these trees are the state after.
    pub block_hash: [u8; 32],
    /// Height of that block.
    pub height: u32,
    /// Block time, in seconds since the Unix epoch.
    pub time: u32,
    /// Sapling commitment tree, if the pool is active at this height.
    pub sapling: Option<WalletPoolTreestate>,
    /// Orchard commitment tree, if the pool is active at this height.
    pub orchard: Option<WalletPoolTreestate>,
    /// Ironwood commitment tree, if the pool is active at this height.
    pub ironwood: Option<WalletPoolTreestate>,
}

impl WalletTreestate {
    pub(crate) fn from_domain(treestate: Treestate) -> Self {
        Self {
            block_hash: treestate.block_hash.into(),
            height: treestate.height.into(),
            time: treestate.time,
            sapling: treestate.sapling.map(WalletPoolTreestate::from_domain),
            orchard: treestate.orchard.map(WalletPoolTreestate::from_domain),
            ironwood: treestate.ironwood.map(WalletPoolTreestate::from_domain),
        }
    }
}

/// A single note-commitment subtree root entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WalletSubtreeRoot {
    /// The root hash of this subtree.
    pub root: [u8; 32],
    /// The hash of the block that completed this subtree.
    pub completing_block_hash: [u8; 32],
    /// The height at which this subtree was completed.
    pub end_height: u32,
}

impl WalletSubtreeRoot {
    pub(crate) fn from_domain(root: SubtreeRoot) -> Self {
        Self {
            root: root.root.into(),
            completing_block_hash: root.completing_block_hash.into(),
            end_height: root.end_height.into(),
        }
    }
}

/// The best chain moved; carries the new tip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WalletTipEvent {
    pub tip: WalletTip,
}

impl WalletTipEvent {
    pub(crate) fn from_domain(event: TipEvent) -> Self {
        Self {
            tip: WalletTip::from_domain(event.tip),
        }
    }
}

/// A mempool delivery, tagged with the tip it was validated against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WalletMempoolEvent {
    pub txid: WalletTxId,
    pub validated_against: WalletTip,
}

impl WalletMempoolEvent {
    pub(crate) fn from_domain(tx: MempoolTx) -> Self {
        Self {
            txid: WalletTxId::from_domain(tx.txid),
            validated_against: WalletTip::from_domain(tx.validated_against),
        }
    }
}

/// Whether a network upgrade is active, pending, or disabled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WalletUpgradeStatus {
    Active,
    Pending,
    Disabled,
}

impl WalletUpgradeStatus {
    fn from_domain(status: UpgradeStatus) -> Self {
        match status {
            UpgradeStatus::Active => Self::Active,
            UpgradeStatus::Pending => Self::Pending,
            UpgradeStatus::Disabled => Self::Disabled,
        }
    }
}

/// One entry of the validator's reported network-upgrade schedule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WalletReportedUpgrade {
    pub branch_id: u32,
    pub name: String,
    pub activation_height: u32,
    pub status: WalletUpgradeStatus,
}

impl WalletReportedUpgrade {
    pub(crate) fn from_domain(upgrade: ReportedUpgrade) -> Self {
        Self {
            branch_id: upgrade.branch_id,
            name: upgrade.name,
            activation_height: upgrade.activation_height.into(),
            status: WalletUpgradeStatus::from_domain(upgrade.status),
        }
    }
}

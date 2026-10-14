//! Stable wallet-facing DTOs.
//!
//! The inner surface's domain types are converted to these here, at the adapter,
//! so the inner primitives can evolve without breaking the embedded wallet. The
//! conversions are one-directional (`from_domain`) and named for direction.

use zaino_primitives::types::{AddressBalance, AddressDelta, BlockRef, TransactionId, Utxo};
use zaino_service::LocatedTxid;

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

//! The compact block and its pool-decomposed transactions — the shape a
//! wallet's note scanner needs (nullifiers, note commitments, ephemeral keys,
//! the scanning prefix of each note's ciphertext), mirrored field-for-field
//! from the domain `CompactBlock`/`PreIndexCompactTx` so no internal type
//! crosses the adapter boundary.

use zaino_primitives::types::{
    ChainMetadata, CompactBlock, OrchardAction, PreIndexCompactTx, SaplingOutput, TransparentInput,
    TransparentOutput,
};

/// A transparent input: reference to the previous output being spent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WalletTransparentInput {
    pub prev_txid: [u8; 32],
    pub prev_index: u32,
}

impl WalletTransparentInput {
    fn from_domain(input: TransparentInput) -> Self {
        Self {
            prev_txid: input.prev_txid.into(),
            prev_index: input.prev_index,
        }
    }
}

/// A transparent output: value and locking script.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WalletTransparentOutput {
    pub value: u64,
    pub script: Vec<u8>,
}

impl WalletTransparentOutput {
    fn from_domain(output: TransparentOutput) -> Self {
        Self {
            value: output.value.into(),
            script: output.script.into(),
        }
    }
}

/// A Sapling output: commitment + detection material.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WalletSaplingOutput {
    pub cmu: [u8; 32],
    pub ephemeral_key: [u8; 32],
    pub enc_ciphertext: [u8; 52],
}

impl WalletSaplingOutput {
    fn from_domain(output: SaplingOutput) -> Self {
        Self {
            cmu: output.cmu.into(),
            ephemeral_key: output.ephemeral_key.into(),
            enc_ciphertext: output.enc_ciphertext.into(),
        }
    }
}

/// An Orchard (or, with the same shape, Ironwood) action: nullifier +
/// commitment + detection material.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WalletOrchardAction {
    pub nullifier: [u8; 32],
    pub cmx: [u8; 32],
    pub ephemeral_key: [u8; 32],
    pub enc_ciphertext: [u8; 52],
}

impl WalletOrchardAction {
    fn from_domain(action: OrchardAction) -> Self {
        Self {
            nullifier: action.nullifier.into(),
            cmx: action.cmx.into(),
            ephemeral_key: action.ephemeral_key.into(),
            enc_ciphertext: action.enc_ciphertext.into(),
        }
    }
}

/// A compact transaction: enough of its structure for a wallet to trial
/// decrypt and detect a payment addressed to it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WalletCompactTx {
    pub txid: [u8; 32],
    pub transparent_inputs: Vec<WalletTransparentInput>,
    pub transparent_outputs: Vec<WalletTransparentOutput>,
    pub sapling_nullifiers: Vec<[u8; 32]>,
    pub sapling_outputs: Vec<WalletSaplingOutput>,
    pub orchard_actions: Vec<WalletOrchardAction>,
    pub ironwood_actions: Vec<WalletOrchardAction>,
}

impl WalletCompactTx {
    fn from_domain(tx: PreIndexCompactTx) -> Self {
        Self {
            txid: tx.txid.into(),
            transparent_inputs: tx
                .transparent_inputs
                .into_iter()
                .map(WalletTransparentInput::from_domain)
                .collect(),
            transparent_outputs: tx
                .transparent_outputs
                .into_iter()
                .map(WalletTransparentOutput::from_domain)
                .collect(),
            sapling_nullifiers: tx.sapling_nullifiers.into_iter().map(Into::into).collect(),
            sapling_outputs: tx
                .sapling_outputs
                .into_iter()
                .map(WalletSaplingOutput::from_domain)
                .collect(),
            orchard_actions: tx
                .orchard_actions
                .into_iter()
                .map(WalletOrchardAction::from_domain)
                .collect(),
            ironwood_actions: tx
                .ironwood_actions
                .into_iter()
                .map(WalletOrchardAction::from_domain)
                .collect(),
        }
    }
}

/// Cumulative note-commitment tree sizes after a block, one count per pool.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WalletChainMetadata {
    pub sapling_tree_size: u32,
    pub orchard_tree_size: u32,
    pub ironwood_tree_size: u32,
}

impl WalletChainMetadata {
    fn from_domain(meta: ChainMetadata) -> Self {
        Self {
            sapling_tree_size: meta.sapling_tree_size.into(),
            orchard_tree_size: meta.orchard_tree_size.into(),
            ironwood_tree_size: meta.ironwood_tree_size.into(),
        }
    }
}

/// A complete compact block, ready for a wallet's note scanner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WalletCompactBlock {
    pub hash: [u8; 32],
    pub prev_hash: [u8; 32],
    pub height: u32,
    pub time: u32,
    /// The block's compact difficulty target (`nBits`), as the raw protocol
    /// encoding.
    pub bits: u32,
    pub transactions: Vec<WalletCompactTx>,
    pub chain_metadata: WalletChainMetadata,
}

impl WalletCompactBlock {
    pub(crate) fn from_domain(block: CompactBlock) -> Self {
        Self {
            hash: block.hash.into(),
            prev_hash: block.prev_hash.into(),
            height: block.height,
            time: block.time,
            bits: block.bits.as_bits(),
            transactions: block
                .transactions
                .into_iter()
                .map(WalletCompactTx::from_domain)
                .collect(),
            chain_metadata: WalletChainMetadata::from_domain(block.chain_metadata),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zaino_primitives::types::{
        BlockHash, CompactDifficulty, Nullifier, TransactionId, Zatoshis,
    };

    fn sample_compact_tx() -> PreIndexCompactTx {
        PreIndexCompactTx {
            txid: TransactionId::from([1u8; 32]),
            transparent_inputs: vec![TransparentInput {
                prev_txid: TransactionId::from([2u8; 32]),
                prev_index: 0,
            }],
            transparent_outputs: vec![TransparentOutput {
                value: Zatoshis::new(1_000).expect("valid amount"),
                script: vec![0xAA].into(),
            }],
            sapling_nullifiers: vec![Nullifier::from([3u8; 32])],
            sapling_outputs: vec![SaplingOutput {
                cmu: [4u8; 32].into(),
                ephemeral_key: [5u8; 32].into(),
                enc_ciphertext: [6u8; 52].into(),
            }],
            orchard_actions: vec![OrchardAction {
                nullifier: [7u8; 32].into(),
                cmx: [8u8; 32].into(),
                ephemeral_key: [9u8; 32].into(),
                enc_ciphertext: [10u8; 52].into(),
            }],
            ironwood_actions: Vec::new(),
        }
    }

    /// Every field of a compact block, including the nested per-pool
    /// transaction structure, survives the domain-to-DTO conversion.
    #[test]
    fn compact_block_maps_every_field() {
        let block = CompactBlock {
            hash: BlockHash::from([1u8; 32]),
            prev_hash: BlockHash::from([2u8; 32]),
            height: 10,
            time: 1_700_000_000,
            bits: CompactDifficulty::try_from_bits(0x2007_ffff).expect("valid difficulty"),
            transactions: vec![sample_compact_tx()],
            chain_metadata: ChainMetadata::new(100u32, 200u32, 0u32),
        };

        let dto = WalletCompactBlock::from_domain(block);

        assert_eq!(dto.hash, [1u8; 32]);
        assert_eq!(dto.prev_hash, [2u8; 32]);
        assert_eq!(dto.height, 10);
        assert_eq!(dto.time, 1_700_000_000);
        assert_eq!(dto.bits, 0x2007_ffff);
        assert_eq!(
            dto.chain_metadata,
            WalletChainMetadata {
                sapling_tree_size: 100,
                orchard_tree_size: 200,
                ironwood_tree_size: 0,
            }
        );

        assert_eq!(dto.transactions.len(), 1);
        let tx = &dto.transactions[0];
        assert_eq!(tx.txid, [1u8; 32]);
        assert_eq!(
            tx.transparent_inputs,
            vec![WalletTransparentInput {
                prev_txid: [2u8; 32],
                prev_index: 0,
            }]
        );
        assert_eq!(
            tx.transparent_outputs,
            vec![WalletTransparentOutput {
                value: 1_000,
                script: vec![0xAA],
            }]
        );
        assert_eq!(tx.sapling_nullifiers, vec![[3u8; 32]]);
        assert_eq!(
            tx.sapling_outputs,
            vec![WalletSaplingOutput {
                cmu: [4u8; 32],
                ephemeral_key: [5u8; 32],
                enc_ciphertext: [6u8; 52],
            }]
        );
        assert_eq!(
            tx.orchard_actions,
            vec![WalletOrchardAction {
                nullifier: [7u8; 32],
                cmx: [8u8; 32],
                ephemeral_key: [9u8; 32],
                enc_ciphertext: [10u8; 52],
            }]
        );
        assert!(tx.ironwood_actions.is_empty());
    }
}

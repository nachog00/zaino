//! Synthesising PR #1638's domain `Block`s from commitments alone.
//!
//! PR #1638's fold (`PoolFold::append_batch` via `TreeStateIndexWriter`) reads
//! only `output.cmu` for sapling and `action.cmx` for orchard/ironwood
//! (`index_writer.rs::PoolBatch::decode`). Every other field — ephemeral key,
//! ciphertext, nullifier, header hash/time, transparent data — is never hashed
//! and never affects a tree root or a subtree boundary. So a faithful input is
//! one transaction per block carrying the block's real commitments in order,
//! with the detection material filled by a constant.
//!
//! Heights are driven contiguously from genesis: the index asserts contiguous
//! ascending heights and keys each record by height (`heights.dat` slot =
//! height), exactly as zainod builds it. A height the source store has no
//! commitments for becomes an empty block (a bare coinbase slot).

use zaino_primitives::types::{
    Block, BlockCommitments, BlockHash, BlockHeader, CompactCiphertext, CompactDifficulty,
    EphemeralKey, EquihashSolution, Height, MerkleRoot, NoteCommitment, Nullifier, OrchardAction,
    OrchardData, SaplingData, SaplingOutput, Transaction, TransactionId, TransparentData,
};

/// Regtest `powLimit` as nBits — a valid compact target (`CompactDifficulty` validates it).
const BITS: u32 = 0x200f_0f0f;

fn detection_ciphertext() -> CompactCiphertext {
    CompactCiphertext::from([0u8; CompactCiphertext::LENGTH])
}

/// A synthetic header for `height`. Hash and time are placeholders (distinct per
/// height); the index records them but no measurement or cross-check reads them.
fn header(height: Height) -> BlockHeader {
    let mut hash = [0u8; 32];
    hash[..4].copy_from_slice(&u32::from(height).to_le_bytes());
    BlockHeader {
        hash: BlockHash::from(hash),
        version: 4,
        prev_hash: BlockHash::ZERO,
        height,
        time: 1_600_000_000u32.saturating_add(u32::from(height)),
        merkle_root: MerkleRoot::from([0u8; 32]),
        block_commitments: BlockCommitments::from([0u8; 32]),
        bits: CompactDifficulty::try_from_bits(BITS).expect("a valid compact target"),
        nonce: [0u8; 32],
        solution: EquihashSolution::Regtest([0u8; 36]),
    }
}

/// One transaction carrying `sapling` outputs and `orchard` actions in order.
///
/// The whole block's commitments ride in this single transaction: within a
/// block the tree appends tx-by-tx then output-by-output, so one transaction
/// holding the already-flattened list yields the identical leaf sequence.
fn transaction(height: Height, sapling: &[[u8; 32]], orchard: &[[u8; 32]]) -> Transaction {
    let outputs = sapling
        .iter()
        .map(|cmu| SaplingOutput {
            cmu: NoteCommitment::from(*cmu),
            ephemeral_key: EphemeralKey::from([0u8; 32]),
            enc_ciphertext: detection_ciphertext(),
        })
        .collect();
    let actions = orchard
        .iter()
        .map(|cmx| OrchardAction {
            nullifier: Nullifier::from([0u8; 32]),
            cmx: NoteCommitment::from(*cmx),
            ephemeral_key: EphemeralKey::from([0u8; 32]),
            enc_ciphertext: detection_ciphertext(),
        })
        .collect();

    let mut txid = [0u8; 32];
    txid[..4].copy_from_slice(&u32::from(height).to_le_bytes());
    Transaction {
        txid: TransactionId::from(txid),
        transparent: TransparentData { coinbase: true, ..TransparentData::default() },
        sprout: Default::default(),
        sapling: SaplingData { outputs, ..Default::default() },
        orchard: OrchardData { actions, ..Default::default() },
        ironwood: OrchardData::default(),
    }
}

/// Build the `Block` at `height` from its sapling and orchard commitments.
pub fn build(height: Height, sapling: &[[u8; 32]], orchard: &[[u8; 32]]) -> Block {
    Block::new(header(height), vec![transaction(height, sapling, orchard)])
}

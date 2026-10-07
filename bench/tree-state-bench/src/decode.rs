//! Decoder for OUR (treestate-bench codebase) on-disk sapling/orchard records.
//!
//! The data source is a synced zaino LMDB store written by the `zaino-indexes`
//! crate. Its named databases `sapling` and `orchard` key by height (8-byte
//! big-endian u64) and store the value exactly as `RecordLayout::encode`
//! produces — no outer framing, no checksum (see
//! `zaino-persistence-codec/src/{lib,layout,keys}.rs`: `put` writes the raw
//! `encode_key`/`encode_value` bytes, and the LMDB backend stores them verbatim).
//!
//! Value layouts (all counts little-endian u32), mirroring
//! `PersistentSaplingValue` / `PersistentOrchardValue`:
//!
//! ```text
//! sapling: tx_count, then per tx:
//!            nullifier_count, nullifier_count × [32]
//!            output_count,    output_count × ( cmu[32] ‖ epk[32] ‖ enc{len u32 ‖ bytes} )
//! orchard: tx_count, then per tx:
//!            action_count, action_count × ( nullifier[32] ‖ cmx[32] ‖ epk[32] ‖ enc{len u32 ‖ bytes} )
//! ```
//!
//! This harness needs only the commitments that become tree leaves: the `cmu`
//! of each sapling output and the `cmx` of each orchard action, in on-disk
//! order (= block append order), which is exactly what PR #1638's fold hashes.

use std::fmt;

/// A malformed on-disk record or key.
#[derive(Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// A read ran past the end of the buffer.
    ShortBuffer { need: usize, at: usize, have: usize },
    /// Bytes remained after the record was fully parsed.
    TrailingBytes { consumed: usize, total: usize },
    /// A height key was not exactly 8 bytes.
    BadKeyWidth { have: usize },
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ShortBuffer { need, at, have } => {
                write!(f, "short buffer: need {need} bytes at offset {at}, have {have}")
            }
            Self::TrailingBytes { consumed, total } => {
                write!(f, "trailing bytes: consumed {consumed} of {total}")
            }
            Self::BadKeyWidth { have } => write!(f, "height key must be 8 bytes, got {have}"),
        }
    }
}

impl std::error::Error for DecodeError {}

/// A bounds-checked forward reader (no panics on short input).
struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        let end = self.pos.checked_add(n).ok_or(DecodeError::ShortBuffer {
            need: n,
            at: self.pos,
            have: self.bytes.len(),
        })?;
        let slice = self.bytes.get(self.pos..end).ok_or(DecodeError::ShortBuffer {
            need: n,
            at: self.pos,
            have: self.bytes.len(),
        })?;
        self.pos = end;
        Ok(slice)
    }

    fn u32_le(&mut self) -> Result<u32, DecodeError> {
        let bytes = self.take(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    /// A little-endian u32 count, widened for looping.
    fn count(&mut self) -> Result<usize, DecodeError> {
        Ok(usize::try_from(self.u32_le()?).unwrap_or(usize::MAX))
    }

    fn array32(&mut self) -> Result<[u8; 32], DecodeError> {
        let slice = self.take(32)?;
        let mut out = [0u8; 32];
        out.copy_from_slice(slice);
        Ok(out)
    }

    fn skip(&mut self, n: usize) -> Result<(), DecodeError> {
        self.take(n).map(|_| ())
    }

    /// Skip a length-framed byte run (u32 little-endian length, then the bytes).
    fn skip_len_prefixed(&mut self) -> Result<(), DecodeError> {
        let len = self.count()?;
        self.skip(len)
    }

    fn finish(self) -> Result<(), DecodeError> {
        if self.pos == self.bytes.len() {
            Ok(())
        } else {
            Err(DecodeError::TrailingBytes { consumed: self.pos, total: self.bytes.len() })
        }
    }
}

/// Decode a height key: 8 bytes, big-endian u64 (`HeightKey`).
pub fn height_from_key(key: &[u8]) -> Result<u64, DecodeError> {
    let bytes: [u8; 8] =
        key.try_into().map_err(|_| DecodeError::BadKeyWidth { have: key.len() })?;
    Ok(u64::from_be_bytes(bytes))
}

/// Every sapling output `cmu` in one block's record, in append order.
pub fn sapling_commitments(value: &[u8]) -> Result<Vec<[u8; 32]>, DecodeError> {
    let mut cursor = Cursor::new(value);
    let mut cmus = Vec::new();
    let tx_count = cursor.count()?;
    for _ in 0..tx_count {
        let nullifier_count = cursor.count()?;
        for _ in 0..nullifier_count {
            cursor.skip(32)?;
        }
        let output_count = cursor.count()?;
        for _ in 0..output_count {
            cmus.push(cursor.array32()?); // cmu
            cursor.skip(32)?; // epk
            cursor.skip_len_prefixed()?; // enc_ciphertext
        }
    }
    cursor.finish()?;
    Ok(cmus)
}

/// Every orchard action `cmx` in one block's record, in append order.
pub fn orchard_commitments(value: &[u8]) -> Result<Vec<[u8; 32]>, DecodeError> {
    let mut cursor = Cursor::new(value);
    let mut cmxs = Vec::new();
    let tx_count = cursor.count()?;
    for _ in 0..tx_count {
        let action_count = cursor.count()?;
        for _ in 0..action_count {
            cursor.skip(32)?; // nullifier
            cmxs.push(cursor.array32()?); // cmx
            cursor.skip(32)?; // epk
            cursor.skip_len_prefixed()?; // enc_ciphertext
        }
    }
    cursor.finish()?;
    Ok(cmxs)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 52-byte compact ciphertext (`CompactCiphertext::LENGTH`), as our codec writes it.
    const ENC: usize = 52;

    fn len_prefixed(byte: u8) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&u32::try_from(ENC).expect("fits").to_le_bytes());
        out.extend_from_slice(&[byte; ENC]);
        out
    }

    /// Golden sapling vector from `zaino-indexes` `value_encodes_to_a_pinned_layout`:
    /// one tx, one nullifier `0x11`, one output with cmu `0x22`, epk `0x33`, enc `0x44`.
    #[test]
    fn sapling_golden_vector_yields_the_one_cmu() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&1u32.to_le_bytes()); // tx_count
        bytes.extend_from_slice(&1u32.to_le_bytes()); // nullifier_count
        bytes.extend_from_slice(&[0x11; 32]);
        bytes.extend_from_slice(&1u32.to_le_bytes()); // output_count
        bytes.extend_from_slice(&[0x22; 32]); // cmu
        bytes.extend_from_slice(&[0x33; 32]); // epk
        bytes.extend_from_slice(&len_prefixed(0x44)); // enc

        assert_eq!(sapling_commitments(&bytes), Ok(vec![[0x22; 32]]));
    }

    /// Golden orchard vector from `zaino-indexes` `value_encodes_to_a_pinned_layout`:
    /// tx0 one action (nf `0xA1`, cmx `0xB2`, epk `0xC3`, enc `0xD4`), tx1 empty.
    #[test]
    fn orchard_golden_vector_yields_the_one_cmx() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&2u32.to_le_bytes()); // tx_count
        bytes.extend_from_slice(&1u32.to_le_bytes()); // tx0 action_count
        bytes.extend_from_slice(&[0xA1; 32]); // nullifier
        bytes.extend_from_slice(&[0xB2; 32]); // cmx
        bytes.extend_from_slice(&[0xC3; 32]); // epk
        bytes.extend_from_slice(&len_prefixed(0xD4)); // enc
        bytes.extend_from_slice(&0u32.to_le_bytes()); // tx1 action_count

        assert_eq!(orchard_commitments(&bytes), Ok(vec![[0xB2; 32]]));
    }

    /// Multiple outputs across multiple txs, order preserved (block append order).
    #[test]
    fn sapling_preserves_order_across_txs() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&2u32.to_le_bytes()); // tx_count
                                                      // tx0: no nullifiers, two outputs
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&2u32.to_le_bytes());
        for (cmu, rest) in [([1u8; 32], [2u8; 32]), ([3u8; 32], [4u8; 32])] {
            bytes.extend_from_slice(&cmu);
            bytes.extend_from_slice(&rest);
            bytes.extend_from_slice(&len_prefixed(0));
        }
        // tx1: one nullifier, one output
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&[9u8; 32]);
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&[5u8; 32]);
        bytes.extend_from_slice(&[6u8; 32]);
        bytes.extend_from_slice(&len_prefixed(0));

        assert_eq!(sapling_commitments(&bytes), Ok(vec![[1u8; 32], [3u8; 32], [5u8; 32]]));
    }

    /// An empty block record (tx_count = 0) decodes to no commitments.
    #[test]
    fn empty_record_is_no_commitments() {
        let bytes = 0u32.to_le_bytes();
        assert_eq!(sapling_commitments(&bytes), Ok(vec![]));
        assert_eq!(orchard_commitments(&bytes), Ok(vec![]));
    }

    #[test]
    fn a_truncated_record_is_rejected_not_panicked() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&1u32.to_le_bytes()); // tx_count
        bytes.extend_from_slice(&0u32.to_le_bytes()); // nullifier_count
        bytes.extend_from_slice(&1u32.to_le_bytes()); // output_count = 1
        bytes.extend_from_slice(&[0x22; 32]); // cmu, then nothing (epk/enc missing)
        assert!(matches!(sapling_commitments(&bytes), Err(DecodeError::ShortBuffer { .. })));
    }

    #[test]
    fn trailing_bytes_are_rejected() {
        let mut bytes = 0u32.to_le_bytes().to_vec(); // tx_count = 0 (complete)
        bytes.push(0xff); // one extra
        assert!(matches!(sapling_commitments(&bytes), Err(DecodeError::TrailingBytes { .. })));
    }

    #[test]
    fn height_key_is_big_endian_eight_bytes() {
        assert_eq!(height_from_key(&[0, 0, 0, 0, 0, 0x06, 0x65, 0x80]), Ok(419_200));
        assert_eq!(height_from_key(&[1, 2, 3]), Err(DecodeError::BadKeyWidth { have: 3 }));
    }
}

//! Read-only loader over OUR synced zaino LMDB store.
//!
//! The store belongs to a *running* zaino; this opens it with
//! `EnvironmentFlags::READ_ONLY` and never writes. Named databases `sapling`
//! and `orchard` are scanned in key order (keys are 8-byte big-endian heights,
//! so byte order is numeric order) and each value is decoded to its leaf
//! commitments.

use std::error::Error;
use std::path::Path;

use lmdb::{Cursor, Environment, EnvironmentFlags, Transaction};

use crate::decode;

/// Per-height leaves for one pool, ascending by height.
pub type PoolLeaves = Vec<(u32, Vec<[u8; 32]>)>;

/// Everything the drive needs from the source store.
pub struct Source {
    pub sapling: PoolLeaves,
    pub orchard: PoolLeaves,
    /// Highest height present in either pool (the chain tip the index builds to).
    pub tip: u32,
    pub sapling_leaves: u64,
    pub orchard_leaves: u64,
}

/// Open `store_dir` read-only and load the sapling and orchard leaves.
///
/// `map_size_bytes` must be at least the environment's on-disk size (a read-only
/// LMDB open maps the whole environment); a value larger than the store is
/// harmless (the mapping is sparse).
pub fn load(
    store_dir: &Path,
    map_size_bytes: usize,
) -> Result<Source, Box<dyn Error + Send + Sync>> {
    let env = Environment::new()
        // Room for the index databases plus the engine's `_watermark` /
        // `_format_versions` bookkeeping, and headroom besides.
        .set_max_dbs(32)
        .set_map_size(map_size_bytes)
        // READ_ONLY: never write the live store. NO_TLS: the read txn may move
        // across threads. NO_READAHEAD: the scan is one linear pass.
        .set_flags(
            EnvironmentFlags::READ_ONLY | EnvironmentFlags::NO_TLS | EnvironmentFlags::NO_READAHEAD,
        )
        .open(store_dir)?;

    let sapling = scan(&env, "sapling", decode::sapling_commitments)?;
    let orchard = scan(&env, "orchard", decode::orchard_commitments)?;

    let tip = sapling
        .last()
        .map(|(h, _)| *h)
        .into_iter()
        .chain(orchard.last().map(|(h, _)| *h))
        .max()
        .ok_or("both sapling and orchard databases are empty")?;

    let sapling_leaves = sapling.iter().map(|(_, l)| u64::try_from(l.len()).unwrap_or(0)).sum();
    let orchard_leaves = orchard.iter().map(|(_, l)| u64::try_from(l.len()).unwrap_or(0)).sum();

    Ok(Source { sapling, orchard, tip, sapling_leaves, orchard_leaves })
}

/// Scan one named database, decoding each value with `decode` and dropping
/// heights whose value carries no commitments (they fold to nothing anyway).
fn scan(
    env: &Environment,
    name: &str,
    decode: impl Fn(&[u8]) -> Result<Vec<[u8; 32]>, decode::DecodeError>,
) -> Result<PoolLeaves, Box<dyn Error + Send + Sync>> {
    let db = env.open_db(Some(name))?;
    let txn = env.begin_ro_txn()?;
    let mut out: PoolLeaves = Vec::new();
    {
        let mut cursor = txn.open_ro_cursor(db)?;
        // lmdb 0.8's `iter_start` yields `(&[u8], &[u8])` directly; a mid-scan
        // LMDB error ends iteration (acceptable for a one-shot bench).
        for (key, value) in cursor.iter_start() {
            let height = decode::height_from_key(key)?;
            let height = u32::try_from(height).map_err(|_| "height exceeds u32")?;
            let leaves = decode(value)?;
            if !leaves.is_empty() {
                out.push((height, leaves));
            }
        }
    }
    drop(txn);
    Ok(out)
}

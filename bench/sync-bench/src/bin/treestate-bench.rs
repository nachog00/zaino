//! Treestate benchmark — how long it takes to build the Sapling and Orchard
//! note-commitment tree states locally from genesis out of a synced zaino LMDB
//! store, and how long it takes to *serve* a tree state at an arbitrary height.
//!
//! Zaino serves treestate and subtree roots by passthrough to Zebra today. This
//! spike measures the cost of computing them locally — the baseline a future
//! treestate index is judged against — by replaying every note commitment the
//! store already holds into an [`incrementalmerkletree`] frontier.
//!
//! Modes (`--mode`):
//!
//! - `serial` — walk heights in order and append every commitment to a per-pool
//!   [`Frontier`], the per-height cost a streaming index would pay. Wall time is
//!   split into store read+decode and leaf-append, and reported in per-1M-height
//!   bands so the growth of append cost with tree size is visible (Sapling
//!   activates at [`SAPLING_ACTIVATION`], Orchard at [`ORCHARD_ACTIVATION`]).
//! - `subtrees` — collect each pool's leaves, compute every complete 2^16-leaf
//!   subtree root in parallel with rayon, then fold the subtree roots and the
//!   trailing partial subtree into the tip root — the shape of a bulk rebuild.
//! - `reads` — the `GetTreeState` serving cost: build the frontier while keeping
//!   a snapshot every N heights, then answer K random heights by cloning the
//!   nearest snapshot at or below the height and replaying the remaining blocks.
//!   Reports p50/p99/mean latency and snapshot memory. `GetTreeState` is a hot
//!   wallet read (librustzcash calls it once per scan batch), so this is the
//!   cost model a local index must beat.
//! - `both` — `serial` then `subtrees` over the same window.
//!
//! The store is opened **read-only with the raw [`lmdb`] crate**, never through
//! `LmdbBackend::open` (which would create namespaces and stamp format versions
//! — writes the live store cannot take). The only path to bytes is the
//! zaino-indexes codecs ([`zaino_persistence_codec::decode_value`]); nothing here
//! hand-parses a record.
//!
//! Correctness (`--zebra-rpc`, genesis windows only): the serial checkpoints and
//! sampled `reads` roots are compared against the validator's `getblock` final
//! roots, and `subtrees` boundary roots against `z_getsubtreesbyindex`. A
//! mismatch fails the run.
#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::time::Instant;

use clap::Parser;

use sync_bench::{init_logging, BoxError};

use crate::driver::{run_pool, Mode, ReadOpts};
use crate::pool::{OrchardPool, SaplingPool};
use crate::verify::Verifier;

/// Mainnet height at which the Sapling pool activates; no Sapling commitment
/// exists below it, so the frontier (and the validator's `finalsaplingroot`) is
/// only meaningful from here on.
const SAPLING_ACTIVATION: u64 = 419_200;

/// Mainnet height at which the Orchard pool activates (NU5).
const ORCHARD_ACTIVATION: u64 = 1_687_104;

/// Leaves per tracked subtree: Zebra tracks a subtree root every 2^16 leaves
/// (`TRACKED_SUBTREE_HEIGHT`), so this is the unit `subtrees` mode parallelises
/// over and the unit `z_getsubtreesbyindex` indexes.
const SUBTREE_SHIFT: u8 = 16;

/// The full note-commitment tree depth for both pools.
const TREE_DEPTH: u8 = 32;

/// Which tree-build cost to measure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum ModeArg {
    /// Per-height streaming append (the index build cost).
    Serial,
    /// Parallel per-subtree rebuild (the bulk build cost).
    Subtrees,
    /// Random-height treestate serving cost.
    Reads,
    /// `serial` then `subtrees`, over the same window.
    Both,
}

/// Which pool(s) to build.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum PoolArg {
    /// Sapling only.
    Sapling,
    /// Orchard only.
    Orchard,
    /// Both pools.
    Both,
}

/// Build Sapling/Orchard tree states locally from a synced zaino LMDB store.
#[derive(Debug, Parser)]
#[command(
    name = "treestate-bench",
    about = "Build Sapling/Orchard tree states locally from a synced zaino LMDB store and measure build and serve cost.",
    long_about = None
)]
struct Args {
    /// The synced store directory (the LMDB environment, opened read-only).
    #[arg(long, env = "STORE_DIR")]
    store_dir: PathBuf,

    /// First height to replay (inclusive). Default: genesis. A non-genesis start
    /// makes the frontier window-relative, so validator comparison is skipped —
    /// the measurement stays valid, only the cross-check is dropped.
    #[arg(long, env = "START")]
    start: Option<u64>,

    /// Last height to replay (inclusive). Default: the store's last height.
    #[arg(long, env = "END")]
    end: Option<u64>,

    /// What to measure.
    #[arg(long, value_enum, env = "MODE", default_value_t = ModeArg::Serial)]
    mode: ModeArg,

    /// Which pool(s) to build.
    #[arg(long, value_enum, env = "POOL", default_value_t = PoolArg::Both)]
    pool: PoolArg,

    /// Validator JSON-RPC endpoint (e.g. `http://zebra...:8232/`). When set and
    /// the window starts at genesis, roots are checked against the validator.
    #[arg(long, env = "ZEBRA_RPC")]
    zebra_rpc: Option<String>,

    /// `reads` mode: keep a frontier snapshot every this many heights.
    #[arg(long, env = "CHECKPOINT_EVERY", default_value_t = 1000)]
    checkpoint_every: u64,

    /// `reads` mode: how many random heights to answer.
    #[arg(long, env = "NUM_READS", default_value_t = 10_000)]
    num_reads: u64,

    /// Virtual address space reserved for the memory map, in GiB. Only an upper
    /// bound on the mapping; must be at least the store's current map size.
    #[arg(long, env = "STORE_MAP_SIZE_GB", default_value_t = 512)]
    map_size_gb: usize,

    /// Open with `MDB_NOLOCK` — for a strictly read-only mount where the lock
    /// table cannot be written. Safe only against a store with no live writer
    /// (e.g. a PVC snapshot); never against the live volume.
    #[arg(long, env = "STORE_NOLOCK", default_value_t = false)]
    nolock: bool,
}

fn main() -> Result<(), BoxError> {
    // The zebra cross-check builds a reqwest client, which never auto-selects a
    // rustls provider in this workspace.
    zaino_common::crypto::ensure_default_crypto_provider();
    init_logging();
    let args = Args::parse();

    if args.checkpoint_every == 0 {
        return Err("--checkpoint-every must be non-zero".into());
    }

    let start = args.start.unwrap_or(0);
    let from_genesis = start == 0;
    let verifier = match (&args.zebra_rpc, from_genesis) {
        (Some(url), true) => Some(Verifier::new(url)?),
        (Some(_), false) => {
            tracing::warn!(
                start,
                "window does not start at genesis; skipping validator cross-check \
                 (frontier roots are window-relative)"
            );
            None
        }
        (None, _) => None,
    };

    let store = store::ReadStore::open(&args.store_dir, args.map_size_gb, args.nolock)?;
    let modes = Mode::selected(args.mode);
    let read_opts = ReadOpts {
        checkpoint_every: args.checkpoint_every,
        num_reads: args.num_reads,
    };

    let started = Instant::now();
    let mut mismatches = 0usize;
    for mode in modes {
        if matches!(args.pool, PoolArg::Sapling | PoolArg::Both) {
            mismatches += run_pool::<SaplingPool>(
                &store,
                mode,
                start,
                args.end,
                verifier.as_ref(),
                read_opts,
            )?;
        }
        if matches!(args.pool, PoolArg::Orchard | PoolArg::Both) {
            mismatches += run_pool::<OrchardPool>(
                &store,
                mode,
                start,
                args.end,
                verifier.as_ref(),
                read_opts,
            )?;
        }
    }

    println!(
        "treestate-bench done in {:.1}s ({} root mismatch(es))",
        started.elapsed().as_secs_f64(),
        mismatches,
    );
    if mismatches > 0 {
        return Err(format!("{mismatches} root comparison(s) did not match the validator").into());
    }
    Ok(())
}

mod leaf {
    //! The tree-leaf abstraction shared by both shielded pools.
    //!
    //! A pool's note commitment is 32 bytes that decode into a field element;
    //! that element is both a tree leaf and (after hashing) a tree node, so one
    //! type serves as the [`Hashable`] the frontier and the subtree reduction
    //! operate over. [`Leaf`] is the seam the pool-generic driver builds against.

    use incrementalmerkletree::Hashable;

    /// A note-commitment tree leaf/node.
    ///
    /// `from_commitment` is the canonical-encoding validation step for a
    /// commitment read off disk; `to_root_bytes` is the inverse, used to compare
    /// a computed root against the validator's.
    pub(crate) trait Leaf: Hashable + Clone + Send + Sync + Sized {
        /// Build a leaf from a commitment's 32 canonical bytes, or `None` if they
        /// are a non-canonical field encoding (a synced store never holds one).
        fn from_commitment(bytes: [u8; 32]) -> Option<Self>;

        /// The node's canonical 32-byte representation, in internal byte order
        /// (what `to_bytes` yields, not display order).
        fn to_root_bytes(&self) -> [u8; 32];
    }

    impl Leaf for sapling_crypto::Node {
        fn from_commitment(bytes: [u8; 32]) -> Option<Self> {
            Self::from_bytes(bytes).into()
        }

        fn to_root_bytes(&self) -> [u8; 32] {
            self.to_bytes()
        }
    }

    impl Leaf for orchard::tree::MerkleHashOrchard {
        fn from_commitment(bytes: [u8; 32]) -> Option<Self> {
            Self::from_bytes(&bytes).into()
        }

        fn to_root_bytes(&self) -> [u8; 32] {
            self.to_bytes()
        }
    }
}

mod tree {
    //! Pure note-commitment tree math over a [`Leaf`].
    //!
    //! One bottom-up reduction serves both jobs `subtrees` mode needs: hashing a
    //! run of leaves into one subtree root, and folding subtree roots into the
    //! tip root. It pads a missing right child at each level with that level's
    //! empty-subtree root, so for a full power-of-two input it is the exact
    //! subtree hash, and for a short input it is the same root a frontier of
    //! those leaves reports.

    use incrementalmerkletree::Level;

    use crate::leaf::Leaf;

    /// Combine `nodes` — all at `start_level` — upward to a single root at
    /// `target_level`.
    ///
    /// A lone node at the right of a level is combined with that level's empty
    /// root, matching how an incremental tree fills the space above its last
    /// leaf. An empty input yields the empty root at `target_level`.
    pub(crate) fn reduce<L: Leaf>(mut nodes: Vec<L>, start_level: u8, target_level: u8) -> L {
        let mut level = start_level;
        while level < target_level {
            let mut next = Vec::with_capacity(nodes.len().div_ceil(2));
            let mut pairs = nodes.chunks_exact(2);
            for pair in &mut pairs {
                next.push(L::combine(Level::new(level), &pair[0], &pair[1]));
            }
            if let [last] = pairs.remainder() {
                let empty = L::empty_root(Level::new(level));
                next.push(L::combine(Level::new(level), last, &empty));
            }
            nodes = next;
            level += 1;
        }
        nodes
            .into_iter()
            .next()
            .unwrap_or_else(|| L::empty_root(Level::new(target_level)))
    }

    /// The root of one subtree of up to `2^shift` leaves, hashed bottom-up.
    pub(crate) fn subtree_root<L: Leaf>(leaves: Vec<L>, shift: u8) -> L {
        reduce(leaves, 0, shift)
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::TREE_DEPTH;
        use incrementalmerkletree::frontier::Frontier;

        /// Distinct, canonical leaves without a PRNG: each is a hash of the
        /// running accumulator, so every value is a valid field element and they
        /// differ.
        fn leaves<L: Leaf>(n: usize) -> Vec<L> {
            let mut out = Vec::with_capacity(n);
            let mut acc = L::empty_leaf();
            for _ in 0..n {
                acc = L::combine(Level::ZERO, &acc, &L::empty_leaf());
                out.push(acc.clone());
            }
            out
        }

        /// The root a plain depth-`TREE_DEPTH` frontier reports after appending
        /// every leaf — the reference the parallel path must reproduce.
        fn frontier_root<L: Leaf>(leaves: &[L]) -> [u8; 32] {
            let mut frontier: Frontier<L, TREE_DEPTH> = Frontier::empty();
            for leaf in leaves {
                assert!(
                    frontier.append(leaf.clone()),
                    "frontier should not fill in tests"
                );
            }
            frontier.root().to_root_bytes()
        }

        /// The root via the `subtrees` path: per-`2^shift` subtree roots, then the
        /// trailing partial subtree, folded from `shift` up to `TREE_DEPTH`.
        fn subtree_path_root<L: Leaf>(leaves: &[L], shift: u8) -> [u8; 32] {
            let size = 1usize << shift;
            let complete = leaves.len() / size * size;
            let mut tops: Vec<L> = leaves[..complete]
                .chunks_exact(size)
                .map(|chunk| subtree_root(chunk.to_vec(), shift))
                .collect();
            if complete < leaves.len() {
                tops.push(subtree_root(leaves[complete..].to_vec(), shift));
            }
            reduce(tops, shift, TREE_DEPTH).to_root_bytes()
        }

        /// Across several lengths (none, exact multiples, partial trailing
        /// subtree) the subtree path and the frontier agree, for both pools.
        fn agree<L: Leaf>() {
            // Small subtree shift so a few thousand leaves span many subtrees.
            const SHIFT: u8 = 8;
            for n in [0usize, 1, 255, 256, 257, 512, 1000, 5000] {
                let ls = leaves::<L>(n);
                assert_eq!(
                    subtree_path_root(&ls, SHIFT),
                    frontier_root(&ls),
                    "pool root mismatch at n={n}"
                );
            }
        }

        #[test]
        fn sapling_subtree_path_matches_frontier() {
            agree::<sapling_crypto::Node>();
        }

        #[test]
        fn orchard_subtree_path_matches_frontier() {
            agree::<orchard::tree::MerkleHashOrchard>();
        }
    }
}

mod store {
    //! Read-only access to the synced store's LMDB environment.
    //!
    //! The environment is opened straight through the `lmdb` crate with
    //! `EnvironmentFlags::READ_ONLY`, never `LmdbBackend::open`: the backend's
    //! open path creates any missing namespace and stamps format versions —
    //! writes the live store cannot take while zaino runs on it. This type only
    //! ever begins read transactions, so it cannot mutate zaino's data. Under
    //! normal locking its one footprint is a slot in LMDB's reader table, the
    //! mechanism by which every LMDB reader coexists with a live writer;
    //! `--nolock` drops even that for a strictly read-only mount (safe only when
    //! no writer is active, e.g. a snapshot).

    use std::path::Path;

    use lmdb::{Cursor as _, Database, Environment, EnvironmentFlags, Transaction as _};
    use lmdb_sys::{MDB_NEXT, MDB_SET_RANGE};

    use sync_bench::BoxError;
    use zaino_persistence::Namespace;

    /// A key/value pair borrowed from a cursor's read transaction.
    type Entry<'txn> = (&'txn [u8], &'txn [u8]);

    /// A read-only handle on the store's LMDB environment.
    pub(crate) struct ReadStore {
        env: Environment,
    }

    impl ReadStore {
        /// Open `dir` read-only. `map_size_gb` reserves virtual address space for
        /// the memory map (an upper bound; must be at least the store's own map
        /// size). `nolock` adds `MDB_NOLOCK`.
        pub(crate) fn open(dir: &Path, map_size_gb: usize, nolock: bool) -> Result<Self, BoxError> {
            let mut flags = EnvironmentFlags::READ_ONLY | EnvironmentFlags::NO_READAHEAD;
            if nolock {
                flags |= EnvironmentFlags::NO_LOCK;
            }
            // The store holds ~a dozen named databases; a generous ceiling spares
            // us tracking the exact set and costs nothing.
            let env = Environment::new()
                .set_max_dbs(64)
                .set_map_size(map_size_gb << 30)
                .set_flags(flags)
                .open(dir)?;
            Ok(Self { env })
        }

        fn open_db(&self, namespace: Namespace) -> Result<Database, BoxError> {
            // `Environment::open_db` opens within a read transaction, so it is
            // valid on a read-only environment (unlike `create_db`, which begins a
            // write transaction and which we never call).
            Ok(self.env.open_db(Some(namespace.as_str()))?)
        }

        /// Visit every value in `namespace` whose key is at or after `start_key`,
        /// in ascending key order (the index codec's big-endian height encoding,
        /// so key order is height order). The visitor returns `false` to stop.
        pub(crate) fn for_each<F>(
            &self,
            namespace: Namespace,
            start_key: &[u8],
            mut visit: F,
        ) -> Result<(), BoxError>
        where
            F: FnMut(&[u8], &[u8]) -> Result<bool, BoxError>,
        {
            let db = self.open_db(namespace)?;
            let txn = self.env.begin_ro_txn()?;
            let cursor = txn.open_ro_cursor(db)?;

            // Seek the first key >= start_key, then step with MDB_NEXT. Raw cursor
            // ops (as the backend uses) so a seek past the end is an empty range,
            // not a panic, and any real LMDB error surfaces instead of being read
            // as the end of the sequence.
            let mut entry = seek(&cursor, Some(start_key), MDB_SET_RANGE)?;
            while let Some((key, value)) = entry {
                if !visit(key, value)? {
                    break;
                }
                entry = seek(&cursor, None, MDB_NEXT)?;
            }
            Ok(())
        }
    }

    /// One cursor step: `Ok(Some(..))` on a hit, `Ok(None)` at the end, `Err` on
    /// a real LMDB failure (never read as the end of the sequence).
    fn seek<'txn>(
        cursor: &lmdb::RoCursor<'txn>,
        key: Option<&[u8]>,
        op: u32,
    ) -> Result<Option<Entry<'txn>>, BoxError> {
        match cursor.get(key, None, op) {
            Ok((Some(k), v)) => Ok(Some((k, v))),
            Ok((None, _)) | Err(lmdb::Error::NotFound) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::pool::{OrchardPool, Pool, SaplingPool};

        use zaino_backend_lmdb::{LmdbBackend, LmdbConfig};
        use zaino_indexes::indexes::orchard::{OrchardBlockValue, OrchardIndex, OrchardTxCompact};
        use zaino_indexes::indexes::sapling::{SaplingBlockValue, SaplingIndex, SaplingTxCompact};
        use zaino_persistence::{Backend, BackendWriter};
        use zaino_persistence_codec::put;
        use zaino_primitives::types::{CompactCiphertext, EphemeralKey, NoteCommitment, Nullifier};
        use zaino_sync::primitives::BlockHeight;

        fn cipher() -> CompactCiphertext {
            CompactCiphertext::from([9u8; CompactCiphertext::LENGTH])
        }

        /// Build a store with two heights of Sapling and Orchard data, then read
        /// it back read-only and confirm the per-height commitments decode in key
        /// order.
        #[test]
        fn read_only_iteration_decodes_in_height_order() {
            let tmp = tempfile::tempdir().expect("tempdir");
            let sapling_ns = SaplingPool::namespace();
            let orchard_ns = OrchardPool::namespace();

            let s_h10 = [0x11u8; 32];
            let s_h11a = [0x22u8; 32];
            let s_h11b = [0x33u8; 32];
            let o_h10 = [0x44u8; 32];
            let o_h11 = [0x55u8; 32];

            {
                let backend = LmdbBackend::open(LmdbConfig {
                    path: tmp.path().to_path_buf(),
                    map_size_bytes: 8 << 20,
                    namespaces: vec![sapling_ns, orchard_ns],
                })
                .expect("open backend");
                let mut writer = backend.writer().expect("writer");

                let sapling_block = |cmus: &[[u8; 32]]| {
                    SaplingBlockValue(vec![SaplingTxCompact {
                        nullifiers: vec![Nullifier::from([1u8; 32])],
                        outputs: cmus
                            .iter()
                            .map(|c| {
                                (
                                    NoteCommitment::from(*c),
                                    EphemeralKey::from([2u8; 32]),
                                    cipher(),
                                )
                            })
                            .collect(),
                    }])
                };
                let orchard_block = |cmx: [u8; 32]| {
                    OrchardBlockValue(vec![OrchardTxCompact {
                        actions: vec![(
                            Nullifier::from([3u8; 32]),
                            NoteCommitment::from(cmx),
                            EphemeralKey::from([4u8; 32]),
                            cipher(),
                        )],
                    }])
                };

                // Heights written out of order so the read path's key ordering is
                // what produces the ascending result.
                writer
                    .commit(vec![
                        put::<SaplingIndex>(
                            sapling_ns,
                            &BlockHeight::new(11),
                            &sapling_block(&[s_h11a, s_h11b]),
                        ),
                        put::<SaplingIndex>(
                            sapling_ns,
                            &BlockHeight::new(10),
                            &sapling_block(&[s_h10]),
                        ),
                        put::<OrchardIndex>(
                            orchard_ns,
                            &BlockHeight::new(11),
                            &orchard_block(o_h11),
                        ),
                        put::<OrchardIndex>(
                            orchard_ns,
                            &BlockHeight::new(10),
                            &orchard_block(o_h10),
                        ),
                    ])
                    .expect("commit");
                backend.flush().expect("flush");
            }

            let store = ReadStore::open(tmp.path(), 1, false).expect("open read store");

            let mut sapling = Vec::new();
            store
                .for_each(sapling_ns, &SaplingPool::start_key(0), |key, value| {
                    let height = SaplingPool::height_of(key).expect("key decodes");
                    for c in SaplingPool::commitments(value).expect("value decodes") {
                        sapling.push((height, c));
                    }
                    Ok(true)
                })
                .expect("scan sapling");
            assert_eq!(
                sapling,
                vec![(10, s_h10), (11, s_h11a), (11, s_h11b)],
                "sapling commitments, height then tx-output order"
            );

            let mut orchard = Vec::new();
            store
                .for_each(orchard_ns, &OrchardPool::start_key(0), |key, value| {
                    let height = OrchardPool::height_of(key).expect("key decodes");
                    for c in OrchardPool::commitments(value).expect("value decodes") {
                        orchard.push((height, c));
                    }
                    Ok(true)
                })
                .expect("scan orchard");
            assert_eq!(orchard, vec![(10, o_h10), (11, o_h11)]);
        }
    }
}

mod pool {
    //! The per-pool specialisation: namespace, activation, codec, and RPC naming.
    //!
    //! Everything the driver does is generic over [`Pool`]; these two zero-sized
    //! impls are the only place Sapling and Orchard differ. Values are decoded
    //! only through the zaino-indexes codecs — the benchmark never hand-parses a
    //! stored record.

    use zaino_indexes::indexes::orchard as orchard_index;
    use zaino_indexes::indexes::sapling as sapling_index;
    use zaino_persistence::Namespace;
    use zaino_persistence_codec::{decode_key, decode_value, encode_key, DecodeError};
    use zaino_sync::primitives::BlockHeight;

    use crate::leaf::Leaf;
    use crate::{ORCHARD_ACTIVATION, SAPLING_ACTIVATION};

    /// One shielded pool's index: how to name it, when it activates, and how to
    /// pull its note commitments out of a stored block value.
    pub(crate) trait Pool {
        /// The tree leaf/node type for this pool.
        type Leaf: Leaf;

        /// The pool's name, used for the LMDB namespace, the RPC `pool` argument,
        /// and log lines.
        const DISPLAY: &'static str;

        /// Mainnet height at which the pool activates; no commitment exists below.
        const ACTIVATION: u64;

        /// The `getblock` field holding this pool's final root.
        const RPC_FINAL_ROOT_FIELD: &'static str;

        /// The LMDB namespace this pool's per-height index lives in.
        fn namespace() -> Namespace;

        /// Every note commitment in a stored block value, in transaction then
        /// output/action order.
        fn commitments(value_bytes: &[u8]) -> Result<Vec<[u8; 32]>, DecodeError>;

        /// The block height a stored key encodes.
        fn height_of(key_bytes: &[u8]) -> Result<u64, DecodeError>;

        /// The encoded key to seek a cursor to `height` (big-endian, so cursor
        /// order is height order).
        fn start_key(height: u64) -> Vec<u8>;
    }

    /// The Sapling pool.
    pub(crate) struct SaplingPool;

    impl Pool for SaplingPool {
        type Leaf = sapling_crypto::Node;
        const DISPLAY: &'static str = "sapling";
        const ACTIVATION: u64 = SAPLING_ACTIVATION;
        const RPC_FINAL_ROOT_FIELD: &'static str = "finalsaplingroot";

        fn namespace() -> Namespace {
            Namespace::from(sapling_index::ID)
        }

        fn commitments(value_bytes: &[u8]) -> Result<Vec<[u8; 32]>, DecodeError> {
            let value = decode_value::<sapling_index::SaplingIndex>(value_bytes)?;
            let mut out = Vec::new();
            for tx in value.0 {
                for (cmu, _epk, _enc) in tx.outputs {
                    out.push(<[u8; 32]>::from(cmu));
                }
            }
            Ok(out)
        }

        fn height_of(key_bytes: &[u8]) -> Result<u64, DecodeError> {
            Ok(decode_key::<sapling_index::SaplingIndex>(key_bytes)?.value())
        }

        fn start_key(height: u64) -> Vec<u8> {
            encode_key::<sapling_index::SaplingIndex>(&BlockHeight::new(height))
        }
    }

    /// The Orchard pool.
    pub(crate) struct OrchardPool;

    impl Pool for OrchardPool {
        type Leaf = orchard::tree::MerkleHashOrchard;
        const DISPLAY: &'static str = "orchard";
        const ACTIVATION: u64 = ORCHARD_ACTIVATION;
        const RPC_FINAL_ROOT_FIELD: &'static str = "finalorchardroot";

        fn namespace() -> Namespace {
            Namespace::from(orchard_index::ID)
        }

        fn commitments(value_bytes: &[u8]) -> Result<Vec<[u8; 32]>, DecodeError> {
            let value = decode_value::<orchard_index::OrchardIndex>(value_bytes)?;
            let mut out = Vec::new();
            for tx in value.0 {
                for (_nf, cmx, _epk, _enc) in tx.actions {
                    out.push(<[u8; 32]>::from(cmx));
                }
            }
            Ok(out)
        }

        fn height_of(key_bytes: &[u8]) -> Result<u64, DecodeError> {
            Ok(decode_key::<orchard_index::OrchardIndex>(key_bytes)?.value())
        }

        fn start_key(height: u64) -> Vec<u8> {
            encode_key::<orchard_index::OrchardIndex>(&BlockHeight::new(height))
        }
    }
}

mod verify {
    //! Cross-checking computed roots against the validator over JSON-RPC.
    //!
    //! A blocking HTTP client is enough: a run makes a handful of calls, far from
    //! the measured loop. `reqwest` is already in the workspace lockfile (the only
    //! HTTP client there), so this adds no new dependency beyond its blocking +
    //! json features.
    //!
    //! # Byte order
    //!
    //! Zebra serialises `getblock`'s `finalsaplingroot` in display (reversed)
    //! order but `finalorchardroot` in internal order, and `z_getsubtreesbyindex`
    //! roots in internal order. Rather than hard-code that asymmetry and risk a
    //! false mismatch on a subtle case, each comparison accepts either orientation
    //! and logs which matched — a genuine root mismatch still fails both.

    use serde_json::{json, Value};

    use sync_bench::BoxError;

    use crate::pool::Pool;

    /// A JSON-RPC cross-checker bound to one validator endpoint.
    pub(crate) struct Verifier {
        client: reqwest::blocking::Client,
        url: String,
    }

    impl Verifier {
        /// Build a checker for the validator at `url`.
        pub(crate) fn new(url: &str) -> Result<Self, BoxError> {
            Ok(Self {
                client: reqwest::blocking::Client::builder().build()?,
                url: url.to_string(),
            })
        }

        /// One JSON-RPC call, returning the `result` value or a typed error.
        fn call(&self, method: &str, params: Value) -> Result<Value, BoxError> {
            let request = json!({
                "jsonrpc": "1.0",
                "id": "treestate-bench",
                "method": method,
                "params": params,
            });
            let response: Value = self
                .client
                .post(&self.url)
                .json(&request)
                .send()?
                .error_for_status()?
                .json()?;
            if let Some(error) = response.get("error") {
                if !error.is_null() {
                    return Err(format!("rpc {method} error: {error}").into());
                }
            }
            response
                .get("result")
                .cloned()
                .ok_or_else(|| format!("rpc {method}: response had no result").into())
        }

        /// Compare the frontier root at `height` against `getblock [height, 1]`'s
        /// final root for the pool. `true` on match.
        pub(crate) fn check_final_root<P: Pool>(
            &self,
            height: u64,
            mine: [u8; 32],
        ) -> Result<bool, BoxError> {
            let result = self.call("getblock", json!([height.to_string(), 1]))?;
            match result.get(P::RPC_FINAL_ROOT_FIELD).and_then(Value::as_str) {
                Some(hex) => compare("final-root", P::DISPLAY, height, hex, mine),
                None => {
                    tracing::warn!(
                        pool = P::DISPLAY,
                        height,
                        field = P::RPC_FINAL_ROOT_FIELD,
                        "validator returned no final root; skipping this checkpoint"
                    );
                    Ok(true)
                }
            }
        }

        /// Compare a complete subtree root against `z_getsubtreesbyindex` at
        /// `index`. `true` on match.
        pub(crate) fn check_subtree_root<P: Pool>(
            &self,
            index: usize,
            mine: [u8; 32],
        ) -> Result<bool, BoxError> {
            let at = u64::try_from(index)?;
            let result = self.call("z_getsubtreesbyindex", json!([P::DISPLAY, at, 1]))?;
            let hex = result
                .get("subtrees")
                .and_then(|subtrees| subtrees.get(0))
                .and_then(|entry| entry.get("root"))
                .and_then(Value::as_str)
                .ok_or_else(|| format!("{}: no subtree root at index {index}", P::DISPLAY))?;
            compare("subtree-root", P::DISPLAY, at, hex, mine)
        }
    }

    /// Compare a validator hex root against the computed bytes in either byte
    /// orientation, printing and logging a MATCH/MISMATCH line.
    fn compare(
        kind: &str,
        pool: &str,
        at: u64,
        validator_hex: &str,
        mine: [u8; 32],
    ) -> Result<bool, BoxError> {
        let got = decode32(validator_hex)?;
        let mut reversed = got;
        reversed.reverse();

        let orientation = if got == mine {
            "direct"
        } else if reversed == mine {
            "reversed"
        } else {
            "none"
        };
        let matched = orientation != "none";

        if matched {
            println!("MATCH    [{pool}] {kind}@{at} ({orientation})");
            tracing::info!(pool, kind, at, orientation, "root MATCH");
        } else {
            let mine_hex = hex::encode(mine);
            println!("MISMATCH [{pool}] {kind}@{at} mine={mine_hex} validator={validator_hex}");
            tracing::error!(pool, kind, at, mine = %mine_hex, validator = %validator_hex, "root MISMATCH");
        }
        Ok(matched)
    }

    /// Decode a 32-byte hex root, rejecting a wrong length.
    fn decode32(hex_str: &str) -> Result<[u8; 32], BoxError> {
        let bytes = hex::decode(hex_str)?;
        <[u8; 32]>::try_from(bytes.as_slice())
            .map_err(|_| format!("expected a 32-byte hex root, got {} bytes", bytes.len()).into())
    }
}

mod driver {
    //! The pool-generic benchmark bodies: `serial` (per-height frontier append),
    //! `subtrees` (parallel per-subtree rebuild), and `reads` (treestate serving),
    //! plus their reporting.
    //!
    //! All drive the same [`Pool`] seam, so Sapling and Orchard run byte-for-byte
    //! the same code. Timings separate the store read+decode cost from the tree
    //! work, since the point is to attribute where local treestate construction —
    //! and serving — spends its time.

    use std::time::{Duration, Instant};

    use incrementalmerkletree::frontier::Frontier;
    use rayon::prelude::*;

    use sync_bench::BoxError;

    use crate::leaf::Leaf;
    use crate::pool::Pool;
    use crate::store::ReadStore;
    use crate::tree::{reduce, subtree_root};
    use crate::verify::Verifier;
    use crate::{ModeArg, SUBTREE_SHIFT, TREE_DEPTH};

    /// Checkpoint the serial frontier root every this many heights (and at the
    /// last height) for the validator cross-check.
    const CHECKPOINT_INTERVAL: u64 = 500_000;

    /// Report serial append cost in bands this many heights wide, to show how the
    /// per-leaf cost evolves as the tree grows.
    const BAND_WIDTH: u64 = 1_000_000;

    /// How many sampled `reads` answers to cross-check against the validator.
    const READ_SPOT_CHECKS: u64 = 5;

    /// One `reads`-mode frontier snapshot: `(replay_from, frontier)`, where the
    /// frontier covers `[start, replay_from - 1]`.
    type Snapshot<P> = (u64, Frontier<<P as Pool>::Leaf, TREE_DEPTH>);

    /// `reads`-mode knobs.
    #[derive(Clone, Copy)]
    pub(crate) struct ReadOpts {
        /// Keep a frontier snapshot every this many heights.
        pub(crate) checkpoint_every: u64,
        /// How many random heights to answer.
        pub(crate) num_reads: u64,
    }

    /// Which cost a single pass measures.
    #[derive(Clone, Copy)]
    pub(crate) enum Mode {
        /// Per-height frontier append.
        Serial,
        /// Parallel per-subtree rebuild.
        Subtrees,
        /// Random-height treestate serving.
        Reads,
    }

    impl Mode {
        /// The passes a CLI `--mode` selects.
        pub(crate) fn selected(arg: ModeArg) -> Vec<Mode> {
            match arg {
                ModeArg::Serial => vec![Mode::Serial],
                ModeArg::Subtrees => vec![Mode::Subtrees],
                ModeArg::Reads => vec![Mode::Reads],
                ModeArg::Both => vec![Mode::Serial, Mode::Subtrees],
            }
        }
    }

    /// Run one pool under one mode; return the number of root comparisons that did
    /// not match the validator (0 when no verifier, or all matched).
    pub(crate) fn run_pool<P: Pool>(
        store: &ReadStore,
        mode: Mode,
        start: u64,
        end: Option<u64>,
        verifier: Option<&Verifier>,
        reads: ReadOpts,
    ) -> Result<usize, BoxError> {
        match mode {
            Mode::Serial => run_serial::<P>(store, start, end, verifier),
            Mode::Subtrees => run_subtrees::<P>(store, start, end, verifier),
            Mode::Reads => run_reads::<P>(store, start, end, verifier, reads),
        }
    }

    /// Leaves/second, computed without an `as` cast. Note counts fit `u32` for
    /// every real chain (Zcash has ~10^8 commitments total); the cap only guards
    /// an impossible overflow rather than ever being hit.
    fn rate(count: u64, elapsed: Duration) -> f64 {
        let secs = elapsed.as_secs_f64();
        if secs <= 0.0 {
            return 0.0;
        }
        f64::from(u32::try_from(count).unwrap_or(u32::MAX)) / secs
    }

    /// Append already-decoded commitments to `frontier`. Isolated from the codec
    /// decode so the serial pass can time leaf-append (field decode + frontier
    /// insert) apart from store read+decode.
    fn append_leaves<P: Pool>(
        frontier: &mut Frontier<P::Leaf, TREE_DEPTH>,
        height: u64,
        commitments: &[[u8; 32]],
    ) -> Result<(), BoxError> {
        for bytes in commitments {
            let leaf = P::Leaf::from_commitment(*bytes).ok_or_else(|| {
                format!(
                    "{}: non-canonical commitment at height {height}",
                    P::DISPLAY
                )
            })?;
            if !frontier.append(leaf) {
                return Err(format!("{}: frontier full at height {height}", P::DISPLAY).into());
            }
        }
        Ok(())
    }

    /// Decode a block value and append every commitment it holds, returning the
    /// count. The shared inner step of the reads pass (which does not split the
    /// decode and append timings).
    fn append_block<P: Pool>(
        frontier: &mut Frontier<P::Leaf, TREE_DEPTH>,
        height: u64,
        value: &[u8],
    ) -> Result<u64, BoxError> {
        let commitments = P::commitments(value)?;
        append_leaves::<P>(frontier, height, &commitments)?;
        Ok(u64::try_from(commitments.len())?)
    }

    /// Accumulates serial-append cost within one 1M-height band.
    struct Band {
        index: u64,
        leaves: u64,
        append: Duration,
        read: Duration,
    }

    impl Band {
        fn new(index: u64) -> Self {
            Self {
                index,
                leaves: 0,
                append: Duration::ZERO,
                read: Duration::ZERO,
            }
        }

        fn report<P: Pool>(&self) {
            let lo = self.index * BAND_WIDTH;
            let hi = lo + BAND_WIDTH - 1;
            println!(
                "  band [{lo:>8}, {hi:>8}] {pool:>7}: {leaves:>10} leaves  \
                 append {append:>7.2}s ({rate:>10.0} leaves/s)  read+decode {read:>7.2}s",
                pool = P::DISPLAY,
                leaves = self.leaves,
                append = self.append.as_secs_f64(),
                rate = rate(self.leaves, self.append),
                read = self.read.as_secs_f64(),
            );
            tracing::info!(
                target: "treestate_bench::band",
                pool = P::DISPLAY,
                band = self.index,
                leaves = self.leaves,
                append_ms = self.append.as_millis(),
                read_ms = self.read.as_millis(),
                "serial band"
            );
        }
    }

    /// The serial pass: walk heights in order, append every commitment to the
    /// frontier, band the cost, and checkpoint the root.
    fn run_serial<P: Pool>(
        store: &ReadStore,
        start: u64,
        end: Option<u64>,
        verifier: Option<&Verifier>,
    ) -> Result<usize, BoxError> {
        println!("serial [{}] from height {start}", P::DISPLAY);
        let mut frontier: Frontier<P::Leaf, TREE_DEPTH> = Frontier::empty();

        let mut total_leaves: u64 = 0;
        let mut read_decode = Duration::ZERO;
        let mut append = Duration::ZERO;
        let mut band: Option<Band> = None;

        let mut next_checkpoint = first_checkpoint(start);
        let mut last_checkpointed: Option<u64> = None;
        let mut last_height = start;
        let mut mismatches = 0usize;

        let wall = Instant::now();
        store.for_each(P::namespace(), &P::start_key(start), |key, value| {
            // Read+decode: height key, end bound, and the block value codec.
            let read_at = Instant::now();
            let height = P::height_of(key)?;
            if end.is_some_and(|e| height > e) {
                return Ok(false);
            }
            let commitments = P::commitments(value)?;
            let read_elapsed = read_at.elapsed();

            // Append: leaf field-decode + frontier insert — the per-height index
            // cost a streaming treestate index would pay.
            let append_at = Instant::now();
            append_leaves::<P>(&mut frontier, height, &commitments)?;
            let append_elapsed = append_at.elapsed();
            let appended = u64::try_from(commitments.len())?;

            total_leaves += appended;
            read_decode += read_elapsed;
            append += append_elapsed;

            // Band rollover, by height.
            let band_index = height / BAND_WIDTH;
            let current = band.get_or_insert_with(|| Band::new(band_index));
            if current.index != band_index {
                current.report::<P>();
                *current = Band::new(band_index);
            }
            current.leaves += appended;
            current.append += append_elapsed;
            current.read += read_elapsed;

            // Checkpoint at exact interval boundaries.
            if height == next_checkpoint {
                mismatches += checkpoint::<P>(&frontier, height, verifier)?;
                last_checkpointed = Some(height);
            }
            if height >= next_checkpoint {
                next_checkpoint = (height / CHECKPOINT_INTERVAL + 1) * CHECKPOINT_INTERVAL;
            }
            last_height = height;
            Ok(true)
        })?;

        if let Some(band) = band {
            band.report::<P>();
        }
        // Always checkpoint the final height, unless it already landed on a
        // boundary.
        if last_checkpointed != Some(last_height) {
            mismatches += checkpoint::<P>(&frontier, last_height, verifier)?;
        }

        let wall = wall.elapsed();
        println!(
            "serial [{pool}] DONE: {leaves} leaves, wall {wall:.1}s | append {append:.1}s \
             ({rate:.0} leaves/s) | read+decode {read:.1}s",
            pool = P::DISPLAY,
            leaves = total_leaves,
            wall = wall.as_secs_f64(),
            append = append.as_secs_f64(),
            rate = rate(total_leaves, append),
            read = read_decode.as_secs_f64(),
        );
        tracing::info!(
            target: "treestate_bench::result",
            mode = "serial",
            pool = P::DISPLAY,
            leaves = total_leaves,
            wall_ms = wall.as_millis(),
            append_ms = append.as_millis(),
            read_ms = read_decode.as_millis(),
            mismatches,
            "serial pass complete"
        );
        Ok(mismatches)
    }

    /// Compute and report the frontier root at `height`, cross-checking it when a
    /// verifier is present and the pool is active there.
    fn checkpoint<P: Pool>(
        frontier: &Frontier<P::Leaf, TREE_DEPTH>,
        height: u64,
        verifier: Option<&Verifier>,
    ) -> Result<usize, BoxError> {
        let root = frontier.root().to_root_bytes();
        println!(
            "  checkpoint [{pool}] h={height:>8} root={root}",
            pool = P::DISPLAY,
            root = hex::encode(root),
        );
        match verifier {
            Some(v) if height >= P::ACTIVATION => {
                if v.check_final_root::<P>(height, root)? {
                    Ok(0)
                } else {
                    Ok(1)
                }
            }
            Some(_) => {
                println!(
                    "  checkpoint [{}] h={height} below activation; validator root is zero, skipped",
                    P::DISPLAY
                );
                Ok(0)
            }
            None => Ok(0),
        }
    }

    /// The first checkpoint height at or after `start` (never genesis).
    fn first_checkpoint(start: u64) -> u64 {
        let aligned = start / CHECKPOINT_INTERVAL * CHECKPOINT_INTERVAL;
        let candidate = if aligned < start {
            aligned + CHECKPOINT_INTERVAL
        } else {
            aligned
        };
        candidate.max(CHECKPOINT_INTERVAL)
    }

    /// The subtrees pass: read the pool's leaves, hash every complete 2^16-leaf
    /// subtree root in parallel, then fold the subtree roots and the trailing
    /// partial subtree into the tip root.
    fn run_subtrees<P: Pool>(
        store: &ReadStore,
        start: u64,
        end: Option<u64>,
        verifier: Option<&Verifier>,
    ) -> Result<usize, BoxError> {
        println!("subtrees [{}] from height {start}", P::DISPLAY);

        // Phase 1 — read every commitment into one pool buffer.
        let read_at = Instant::now();
        let leaves = collect_leaves::<P>(store, start, end)?;
        let read_decode = read_at.elapsed();
        let total = leaves.len();

        // Phase 2 — complete subtree roots, in parallel.
        let size = 1usize << SUBTREE_SHIFT;
        let parallel_at = Instant::now();
        let complete_roots: Vec<P::Leaf> = leaves
            .par_chunks_exact(size)
            .map(|chunk| -> Result<P::Leaf, BoxError> {
                Ok(subtree_root(decode_leaves::<P>(chunk)?, SUBTREE_SHIFT))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let parallel = parallel_at.elapsed();

        // Phase 3 — trailing partial subtree, then fold to the tip.
        let fold_at = Instant::now();
        let complete = complete_roots.len() * size;
        let mut tops = complete_roots.clone();
        if complete < total {
            tops.push(subtree_root(
                decode_leaves::<P>(&leaves[complete..])?,
                SUBTREE_SHIFT,
            ));
        }
        let tip = reduce(tops, SUBTREE_SHIFT, TREE_DEPTH).to_root_bytes();
        let fold = fold_at.elapsed();

        println!(
            "subtrees [{pool}] DONE: {total} leaves, {subs} complete subtrees | \
             read+decode {read:.1}s | parallel {parallel:.1}s | fold {fold:.3}s | tip={tip}",
            pool = P::DISPLAY,
            subs = complete_roots.len(),
            read = read_decode.as_secs_f64(),
            parallel = parallel.as_secs_f64(),
            fold = fold.as_secs_f64(),
            tip = hex::encode(tip),
        );
        tracing::info!(
            target: "treestate_bench::result",
            mode = "subtrees",
            pool = P::DISPLAY,
            leaves = total,
            complete_subtrees = complete_roots.len(),
            read_ms = read_decode.as_millis(),
            parallel_ms = parallel.as_millis(),
            fold_ms = fold.as_millis(),
            "subtrees pass complete"
        );

        // Cross-check the first 3 and last 2 complete subtree roots.
        let mut mismatches = 0usize;
        if let Some(v) = verifier {
            for index in boundary_indices(complete_roots.len()) {
                if !v.check_subtree_root::<P>(index, complete_roots[index].to_root_bytes())? {
                    mismatches += 1;
                }
            }
        }
        Ok(mismatches)
    }

    /// The reads pass: build the frontier while snapshotting it every N heights,
    /// then answer K random heights from the nearest snapshot and report the
    /// serving-latency distribution.
    fn run_reads<P: Pool>(
        store: &ReadStore,
        start: u64,
        end: Option<u64>,
        verifier: Option<&Verifier>,
        opts: ReadOpts,
    ) -> Result<usize, BoxError> {
        println!(
            "reads [{}] from height {start}, snapshot every {} heights, {} reads",
            P::DISPLAY,
            opts.checkpoint_every,
            opts.num_reads,
        );

        // Phase 1 — build, snapshotting the frontier every `checkpoint_every`
        // heights. A snapshot is `(replay_from, frontier)`: the frontier covers
        // `[start, replay_from - 1]`, so answering height `h` means cloning the
        // snapshot with the greatest `replay_from <= h + 1` and replaying
        // `[replay_from, h]`. The seed covers nothing, so it serves any `h`.
        let build_at = Instant::now();
        let mut frontier: Frontier<P::Leaf, TREE_DEPTH> = Frontier::empty();
        let mut snapshots: Vec<Snapshot<P>> = vec![(start, Frontier::empty())];
        let mut last_height = start;
        let mut total_leaves: u64 = 0;

        store.for_each(P::namespace(), &P::start_key(start), |key, value| {
            let height = P::height_of(key)?;
            if end.is_some_and(|e| height > e) {
                return Ok(false);
            }
            total_leaves += append_block::<P>(&mut frontier, height, value)?;
            if height % opts.checkpoint_every == 0 {
                snapshots.push((height + 1, frontier.clone()));
            }
            last_height = height;
            Ok(true)
        })?;
        let build = build_at.elapsed();

        let snapshot_bytes: usize = snapshots
            .iter()
            .map(|(_, f)| {
                std::mem::size_of::<Frontier<P::Leaf, TREE_DEPTH>>() + f.dynamic_memory_usage()
            })
            .sum();
        let per_snapshot = snapshot_bytes / snapshots.len().max(1);
        println!(
            "reads [{pool}] built {leaves} leaves in {build:.1}s | {snaps} snapshots \
             (~{per} B each, {total} B total) every {n} heights",
            pool = P::DISPLAY,
            leaves = total_leaves,
            build = build.as_secs_f64(),
            snaps = snapshots.len(),
            per = per_snapshot,
            total = snapshot_bytes,
            n = opts.checkpoint_every,
        );

        // Phase 2 — answer random heights over the pool's active range.
        let lo = start.max(P::ACTIVATION);
        if last_height < lo {
            println!(
                "reads [{}]: active range [{lo}, {last_height}] is empty; no reads to time",
                P::DISPLAY
            );
            return Ok(0);
        }
        let span = last_height - lo + 1;
        let spot_every = (opts.num_reads / READ_SPOT_CHECKS).max(1);
        let mut rng = SplitMix64::new(0x7265_6164_0000_0000 ^ P::ACTIVATION);
        let mut latencies: Vec<Duration> = Vec::with_capacity(usize::try_from(opts.num_reads)?);
        let mut mismatches = 0usize;

        for i in 0..opts.num_reads {
            let height = lo + rng.next_u64() % span;
            let answer_at = Instant::now();
            let root = answer::<P>(store, &snapshots, height)?;
            latencies.push(answer_at.elapsed());

            // Spot-check a handful of the served roots against the validator.
            if i % spot_every == 0 {
                if let Some(v) = verifier {
                    if height >= P::ACTIVATION && !v.check_final_root::<P>(height, root)? {
                        mismatches += 1;
                    }
                }
            }
        }

        report_latencies::<P>(&mut latencies);
        Ok(mismatches)
    }

    /// Answer one `GetTreeState`: clone the nearest snapshot at or below `height`
    /// and replay the remaining blocks' commitments, returning the frontier root.
    fn answer<P: Pool>(
        store: &ReadStore,
        snapshots: &[Snapshot<P>],
        height: u64,
    ) -> Result<[u8; 32], BoxError> {
        // The last snapshot whose coverage does not exceed `height`. The seed's
        // `replay_from == start <= height`, so there is always at least one.
        let idx = snapshots
            .partition_point(|(replay_from, _)| *replay_from <= height + 1)
            .saturating_sub(1);
        let (replay_from, base) = &snapshots[idx];
        let mut frontier = base.clone();
        store.for_each(P::namespace(), &P::start_key(*replay_from), |key, value| {
            let at = P::height_of(key)?;
            if at > height {
                return Ok(false);
            }
            append_block::<P>(&mut frontier, at, value)?;
            Ok(true)
        })?;
        Ok(frontier.root().to_root_bytes())
    }

    /// Print and log the latency distribution of a reads pass.
    fn report_latencies<P: Pool>(latencies: &mut [Duration]) {
        if latencies.is_empty() {
            return;
        }
        latencies.sort_unstable();
        let n = latencies.len();
        let p50 = latencies[n / 2];
        let p99 = latencies[(n * 99 / 100).min(n - 1)];
        let sum: Duration = latencies.iter().sum();
        let mean = sum / u32::try_from(n).unwrap_or(u32::MAX);
        let micros = |d: Duration| d.as_secs_f64() * 1e6;

        println!(
            "reads [{pool}] latency over {n} reads: p50 {p50:.1}us  p99 {p99:.1}us  mean {mean:.1}us",
            pool = P::DISPLAY,
            p50 = micros(p50),
            p99 = micros(p99),
            mean = micros(mean),
        );
        tracing::info!(
            target: "treestate_bench::result",
            mode = "reads",
            pool = P::DISPLAY,
            reads = n,
            p50_us = micros(p50),
            p99_us = micros(p99),
            mean_us = micros(mean),
            "reads pass complete"
        );
    }

    /// Read every commitment in `[start, end]` into one buffer.
    fn collect_leaves<P: Pool>(
        store: &ReadStore,
        start: u64,
        end: Option<u64>,
    ) -> Result<Vec<[u8; 32]>, BoxError> {
        let mut leaves: Vec<[u8; 32]> = Vec::new();
        store.for_each(P::namespace(), &P::start_key(start), |key, value| {
            let height = P::height_of(key)?;
            if end.is_some_and(|e| height > e) {
                return Ok(false);
            }
            leaves.extend(P::commitments(value)?);
            Ok(true)
        })?;
        Ok(leaves)
    }

    /// Decode a run of commitment bytes into leaves, rejecting non-canonical ones.
    fn decode_leaves<P: Pool>(bytes: &[[u8; 32]]) -> Result<Vec<P::Leaf>, BoxError> {
        bytes
            .iter()
            .map(|b| {
                P::Leaf::from_commitment(*b)
                    .ok_or_else(|| BoxError::from("non-canonical commitment"))
            })
            .collect()
    }

    /// The first three and last two indices of `n` complete subtrees, deduplicated
    /// and in order; empty when there are no complete subtrees.
    fn boundary_indices(n: usize) -> Vec<usize> {
        let mut indices: Vec<usize> = Vec::new();
        for i in 0..n.min(3) {
            indices.push(i);
        }
        for i in [n.saturating_sub(2), n.saturating_sub(1)] {
            if i < n && !indices.contains(&i) {
                indices.push(i);
            }
        }
        indices
    }

    /// A tiny deterministic PRNG for reproducible read-height selection, so the
    /// read workload is the same across runs without pulling in `rand`.
    struct SplitMix64(u64);

    impl SplitMix64 {
        fn new(seed: u64) -> Self {
            Self(seed)
        }

        fn next_u64(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn boundary_indices_cover_ends_without_duplicates() {
            assert_eq!(boundary_indices(0), Vec::<usize>::new());
            assert_eq!(boundary_indices(1), vec![0]);
            assert_eq!(boundary_indices(2), vec![0, 1]);
            assert_eq!(boundary_indices(4), vec![0, 1, 2, 3]);
            assert_eq!(boundary_indices(100), vec![0, 1, 2, 98, 99]);
        }

        #[test]
        fn first_checkpoint_is_never_genesis() {
            assert_eq!(first_checkpoint(0), CHECKPOINT_INTERVAL);
            assert_eq!(first_checkpoint(1), CHECKPOINT_INTERVAL);
            assert_eq!(first_checkpoint(CHECKPOINT_INTERVAL), CHECKPOINT_INTERVAL);
            assert_eq!(
                first_checkpoint(CHECKPOINT_INTERVAL + 1),
                CHECKPOINT_INTERVAL * 2
            );
        }

        #[test]
        fn splitmix_is_deterministic_and_spread() {
            let mut a = SplitMix64::new(42);
            let mut b = SplitMix64::new(42);
            let xs: Vec<u64> = (0..8).map(|_| a.next_u64()).collect();
            let ys: Vec<u64> = (0..8).map(|_| b.next_u64()).collect();
            assert_eq!(xs, ys, "same seed, same sequence");
            assert!(xs.windows(2).all(|w| w[0] != w[1]), "no immediate repeats");
        }
    }
}

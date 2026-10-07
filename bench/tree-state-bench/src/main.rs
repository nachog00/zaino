//! Measurement harness for PR #1638's tree-state index (Eli Barbieri's design),
//! timed on real mainnet data so it can be compared against our own treestate
//! bench.
//!
//! Measurement path: PREFERRED. The harness drives #1638's real public writer
//! and store (`TreeStateIndexWriter` over a `DiskEngine`/`DiskStore`, as zainod
//! wires it) with domain `Block`s streamed in chain order from genesis, then
//! reads back through `TreeStateService` / `ReadView` — the same fold
//! (`append_batch_visiting` / `combine_pairs`), node retention, commit path and
//! serving path production uses. The blocks carry the real note commitments from
//! our synced LMDB store and nothing else, which is faithful because the fold
//! hashes only `cmu`/`cmx` (see `blocks.rs`).
//!
//! What it does NOT include: the gRPC wire layer, the validator/producer fetch,
//! the `synced` gate (one atomic load, excluded from the read hot path), and the
//! real block headers/ciphertexts (never hashed).
//!
//! Env: `STORE_DIR` (our LMDB store, read-only), `WORK_DIR` (where #1638's index
//! is written, must be empty), `ZEBRA_RPC` (optional; enables the cross-check),
//! `K` (random read count, default 10000), `MAP_SIZE_BYTES`, `BATCH_BYTES`,
//! `QUEUE_BYTES`.

#![forbid(unsafe_code)]

mod blocks;
mod decode;
mod store;
mod zebra;

use std::env;
use std::error::Error;
use std::hint::black_box;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use zaino_index_tree_state::{schema, PoolActivations, TreeStateIndexWriter, TreeStateService};
use zaino_persistence::{disk_bytes, fs::RealFs, DiskEngine, IndexKind, PersistenceEngine};
use zaino_primitives::types::{Height, ShieldedPool};
use zaino_sync::{BlockSink, Step};
use zcash_protocol::consensus::NetworkType;

/// Mainnet Sapling activation (first height with a sapling tree state).
const SAPLING_ACTIVATION: u32 = 419_200;
/// Mainnet NU5 activation (Orchard).
const NU5_ACTIVATION: u32 = 1_687_104;
/// Height band for per-band build timing.
const BAND: u32 = 1_000_000;

fn env_bytes(name: &str, default: usize) -> Result<NonZeroUsize, Box<dyn Error + Send + Sync>> {
    let value = match env::var(name) {
        Ok(raw) => raw.parse::<usize>()?,
        Err(_) => default,
    };
    NonZeroUsize::new(value).ok_or_else(|| format!("{name} must be non-zero").into())
}

fn height(h: u32) -> Height {
    Height::try_from(h).expect("height within protocol range")
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let store_dir = PathBuf::from(env::var("STORE_DIR").map_err(|_| "STORE_DIR is required")?);
    let work_dir = PathBuf::from(env::var("WORK_DIR").map_err(|_| "WORK_DIR is required")?);
    let zebra_rpc = env::var("ZEBRA_RPC").ok();
    let k: usize = env::var("K").ok().map_or(Ok(10_000), |raw| raw.parse())?;
    let map_size = env_bytes("MAP_SIZE_BYTES", 64usize << 30)?.get();
    let batch_bytes = env_bytes("BATCH_BYTES", 128usize << 20)?;
    let queue_bytes = env_bytes("QUEUE_BYTES", 256usize << 20)?;

    println!("# tree-state-bench (PR #1638), PREFERRED path");
    println!("STORE_DIR={}", store_dir.display());
    println!("WORK_DIR={}", work_dir.display());
    println!("K={k}  BATCH_BYTES={}  QUEUE_BYTES={}", batch_bytes.get(), queue_bytes.get());

    prepare_work_dir(&work_dir)?;

    // --- Load the source commitments (blocking LMDB scan + decode). ---
    let load_start = Instant::now();
    let loader = {
        let store_dir = store_dir.clone();
        tokio::task::spawn_blocking(move || store::load(&store_dir, map_size)).await?
    };
    let source = loader?;
    println!(
        "\n## source\nload: {:?}\ntip height: {}\nsapling leaves: {}\norchard leaves: {}",
        load_start.elapsed(),
        source.tip,
        source.sapling_leaves,
        source.orchard_leaves,
    );

    // --- Open #1638's real store + writer. ---
    let store = DiskEngine::new(RealFs::shared()).open(&work_dir, &schema(NetworkType::Main))?;
    let index = TreeStateIndexWriter::new(store, batch_bytes)?;

    let activations = PoolActivations {
        sapling: height(SAPLING_ACTIVATION),
        orchard: Some(height(NU5_ACTIVATION)),
        ironwood: None,
    };
    let served = index.published().served();
    let service = TreeStateService::new(index.published().served(), NetworkType::Main, activations);
    let durable_rx = index.published().subscribe_finalized();

    let sink = BlockSink::new("blocks");
    let subscription = {
        let mut sink = sink;
        let subscription = sink.subscribe(IndexKind::TreeState.name(), queue_bytes);
        (sink, subscription)
    };
    let (sink, subscription) = subscription;
    let running = tokio::spawn(index.run(subscription));

    // --- Band-timing monitor: durable tip crossing each 1M boundary. ---
    let build_start = Instant::now();
    let monitor = {
        let mut durable_rx = durable_rx;
        tokio::spawn(async move {
            let mut crossings: Vec<(u32, Duration)> = Vec::new();
            let mut next = BAND;
            loop {
                if let Some(h) = durable_rx.borrow_and_update().map(u32::from) {
                    while h >= next {
                        crossings.push((next, build_start.elapsed()));
                        next = next.saturating_add(BAND);
                    }
                }
                if durable_rx.changed().await.is_err() {
                    break;
                }
            }
            crossings
        })
    };

    // --- Producer: contiguous blocks from genesis to the tip. ---
    let mut si = 0usize;
    let mut oi = 0usize;
    for h in 0..=source.tip {
        let sapling = take(&source.sapling, &mut si, h);
        let orchard = take(&source.orchard, &mut oi, h);
        let block = Arc::new(blocks::build(height(h), sapling, orchard));
        sink.send(Step::Apply { height: height(h), finalized: true, data: block }).await;
    }
    sink.shutdown();
    running.await?;
    let total = build_start.elapsed();
    let crossings = monitor.await?;

    report_build(&source, total, &crossings);

    // --- On-disk size. ---
    let bytes = disk_bytes(&work_dir)?;
    println!("\n## on-disk size\n{bytes} bytes ({:.1} MiB)", mib(bytes));

    // --- Read latency. ---
    report_reads(&service, &served, &source, k)?;

    // --- Correctness vs zebra. ---
    let mut mismatch = false;
    match zebra_rpc {
        Some(url) => {
            let rpc = zebra::Rpc::new(&url)?;
            println!("\n## correctness (ZEBRA_RPC={url})");
            mismatch |= check_tree_states(&rpc, &service, source.tip)?;
            mismatch |= check_subtrees(&rpc, &served, ShieldedPool::Sapling, "sapling")?;
            mismatch |= check_subtrees(&rpc, &served, ShieldedPool::Orchard, "orchard")?;
        }
        None => println!("\n## correctness\nskipped (ZEBRA_RPC unset)"),
    }

    if mismatch {
        eprintln!("\nRESULT: MISMATCH (see above)");
        std::process::exit(1);
    }
    println!("\nRESULT: OK");
    Ok(())
}

/// Leaves of the pool at `h`, advancing the cursor past it; `&[]` if absent.
fn take<'a>(pool: &'a store::PoolLeaves, cursor: &mut usize, h: u32) -> &'a [[u8; 32]] {
    if let Some((height, leaves)) = pool.get(*cursor) {
        if *height == h {
            *cursor += 1;
            return leaves;
        }
    }
    &[]
}

/// Create `WORK_DIR` if absent and refuse to build over an existing index.
fn prepare_work_dir(work_dir: &std::path::Path) -> Result<(), Box<dyn Error + Send + Sync>> {
    std::fs::create_dir_all(work_dir)?;
    if work_dir.join("MANIFEST").exists() {
        return Err("WORK_DIR already holds an index (MANIFEST present); use a fresh dir".into());
    }
    Ok(())
}

fn mib(bytes: u64) -> f64 {
    let bytes = u32::try_from(bytes.min(u64::from(u32::MAX))).unwrap_or(u32::MAX);
    f64::from(bytes) / (1024.0 * 1024.0)
}

fn report_build(source: &store::Source, total: Duration, crossings: &[(u32, Duration)]) {
    let total_leaves = source.sapling_leaves + source.orchard_leaves;
    println!("\n## build (genesis -> tip, all pools folded together)");
    println!("total wall time: {total:?}");
    let secs = total.as_secs_f64().max(f64::MIN_POSITIVE);
    println!("leaves/s (sapling+orchard): {:.0}", as_f64(total_leaves) / secs);
    println!("sapling leaves/s (overall): {:.0}", as_f64(source.sapling_leaves) / secs);
    println!("orchard leaves/s (overall): {:.0}", as_f64(source.orchard_leaves) / secs);

    // Per-1M-height band leaves.
    let bands = usize::try_from(source.tip / BAND).unwrap_or(0) + 2;
    let mut band_leaves = vec![0u64; bands];
    for pool in [&source.sapling, &source.orchard] {
        for (h, leaves) in pool {
            let idx = usize::try_from(h / BAND).unwrap_or(0);
            band_leaves[idx] += u64::try_from(leaves.len()).unwrap_or(0);
        }
    }

    println!("\nper-1M-height band (band k = heights [k*1M, (k+1)*1M)):");
    let mut prev = Duration::ZERO;
    for (band, (boundary, at)) in crossings.iter().enumerate() {
        let dur = at.saturating_sub(prev);
        let leaves = band_leaves.get(band).copied().unwrap_or(0);
        println!(
            "  band {band} (<{}M): {dur:?}  {} leaves  {:.0} leaves/s",
            boundary / BAND,
            leaves,
            as_f64(leaves) / dur.as_secs_f64().max(f64::MIN_POSITIVE),
        );
        prev = *at;
    }
    let tail = crossings.len();
    let leaves = band_leaves.get(tail).copied().unwrap_or(0);
    let dur = total.saturating_sub(prev);
    println!(
        "  band {tail} (tail to tip): {dur:?}  {} leaves  {:.0} leaves/s",
        leaves,
        as_f64(leaves) / dur.as_secs_f64().max(f64::MIN_POSITIVE),
    );
}

fn as_f64(value: u64) -> f64 {
    u32::try_from(value / 1000).map_or(f64::from(u32::MAX), f64::from) * 1000.0
        + f64::from(u32::try_from(value % 1000).unwrap_or(0))
}

/// Deterministic splitmix64 step.
fn splitmix(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

fn report_reads(
    service: &TreeStateService<zaino_persistence::DiskView>,
    served: &zaino_sync::Served<zaino_index_tree_state::ReadView<zaino_persistence::DiskView>>,
    source: &store::Source,
    k: usize,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let lo = SAPLING_ACTIVATION;
    let hi = source.tip;
    let span = u64::from(hi.saturating_sub(lo)) + 1;
    let mut state = 0x1234_5678_9ABC_DEF0u64;
    let heights: Vec<Height> = (0..k)
        .map(|_| {
            let offset = u32::try_from(splitmix(&mut state) % span).unwrap_or(0);
            height(lo.saturating_add(offset))
        })
        .collect();

    println!("\n## read latency: {k} random GetTreeState, through TreeStateService");
    // First-touch (pages resident from the build, this access pattern cold) then resident.
    let first = time_reads(service, &heights)?;
    let resident = time_reads(service, &heights)?;
    print_latency("warm first-touch", &first);
    print_latency("warm resident", &resident);
    println!(
        "note: a truly cold read (page cache dropped) needs eviction from a separate process\n\
         (posix_fadvise(POSIX_FADV_DONTNEED) or drop_caches); the design doc measures 296 us cold\n\
         vs 3.0 us warm — not reproducible in-process (DONTNEED cannot evict pages mapped here)."
    );

    // Full GetSubtreeRoots list.
    for (pool, name) in [(ShieldedPool::Sapling, "sapling"), (ShieldedPool::Orchard, "orchard")] {
        let start = Instant::now();
        let roots = served.pin_any().subtree_roots(pool, 0, 0)?;
        let elapsed = start.elapsed();
        println!("GetSubtreeRoots {name}: {} roots in {elapsed:?}", roots.len());
    }
    Ok(())
}

fn time_reads(
    service: &TreeStateService<zaino_persistence::DiskView>,
    heights: &[Height],
) -> Result<Vec<Duration>, Box<dyn Error + Send + Sync>> {
    let mut samples = Vec::with_capacity(heights.len());
    let mut acc = 0u8;
    for &h in heights {
        let start = Instant::now();
        let tree = service.treestate(h)?;
        samples.push(start.elapsed());
        acc ^= tree.sapling.as_bytes().first().copied().unwrap_or(0);
    }
    black_box(acc);
    Ok(samples)
}

fn print_latency(label: &str, samples: &[Duration]) {
    if samples.is_empty() {
        println!("{label}: no samples");
        return;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let p = |q: usize| sorted[(sorted.len() * q / 100).min(sorted.len() - 1)];
    let sum: Duration = sorted.iter().sum();
    let mean = sum / u32::try_from(sorted.len()).unwrap_or(1);
    println!("{label}: p50={:?} p99={:?} mean={mean:?}", p(50), p(99));
}

/// Compare tree-state roots at 500k checkpoints + tip. Returns true on any mismatch.
fn check_tree_states(
    rpc: &zebra::Rpc,
    service: &TreeStateService<zaino_persistence::DiskView>,
    tip: u32,
) -> Result<bool, Box<dyn Error + Send + Sync>> {
    let mut checkpoints: Vec<u32> = (1..).map(|n| n * 500_000).take_while(|h| *h <= tip).collect();
    checkpoints.push(tip);
    checkpoints.dedup();

    let mut mismatch = false;
    for h in checkpoints {
        if h < SAPLING_ACTIVATION {
            continue;
        }
        let tree = service.treestate(height(h))?;
        let (zebra_sapling, zebra_orchard) = rpc.block_roots(h)?;

        mismatch |= compare_root(
            &format!("treestate {h} sapling"),
            zebra_sapling.as_deref(),
            &zebra::sapling_root(tree.sapling.as_bytes())?,
        );
        if h >= NU5_ACTIVATION {
            mismatch |= compare_root(
                &format!("treestate {h} orchard"),
                zebra_orchard.as_deref(),
                &zebra::orchard_root(tree.orchard.as_bytes())?,
            );
        }
    }
    Ok(mismatch)
}

fn compare_root(label: &str, zebra_hex: Option<&str>, internal: &[u8; 32]) -> bool {
    match zebra_hex {
        None => {
            println!("  MISMATCH {label}: zebra returned no root");
            true
        }
        Some(zebra_hex) => match zebra::orientation_match(zebra_hex, internal) {
            Some(orientation) => {
                println!("  MATCH    {label} ({orientation})");
                false
            }
            None => {
                println!("  MISMATCH {label}: ours={} zebra={zebra_hex}", hex::encode(internal));
                true
            }
        },
    }
}

/// Compare the first 3 and last 2 subtree roots against zebra. Returns true on any mismatch.
fn check_subtrees(
    rpc: &zebra::Rpc,
    served: &zaino_sync::Served<zaino_index_tree_state::ReadView<zaino_persistence::DiskView>>,
    pool: ShieldedPool,
    name: &str,
) -> Result<bool, Box<dyn Error + Send + Sync>> {
    let ours = served.pin_any().subtree_roots(pool, 0, 0)?;
    if ours.is_empty() {
        println!("  (no {name} subtrees)");
        return Ok(false);
    }
    let count = u64::try_from(ours.len()).unwrap_or(0);
    let theirs = rpc.subtrees(name, count)?;

    let mut mismatch = false;
    let last = ours.len().saturating_sub(1);
    let mut indices = vec![0usize, 1, 2, last.saturating_sub(1), last];
    indices.retain(|i| *i < ours.len());
    indices.sort_unstable();
    indices.dedup();

    for i in indices {
        let our_root = <[u8; 32]>::from(ours[i].root);
        let our_height = u32::from(ours[i].completing.height);
        match theirs.get(i) {
            None => {
                println!("  MISMATCH {name} subtree {i}: zebra has no entry");
                mismatch = true;
            }
            Some((zebra_root, zebra_height)) => {
                let root_ok = zebra::orientation_match(zebra_root, &our_root);
                let height_ok = *zebra_height == our_height;
                match (root_ok, height_ok) {
                    (Some(orientation), true) => {
                        println!(
                            "  MATCH    {name} subtree {i} (root {orientation}, end {our_height})"
                        )
                    }
                    _ => {
                        println!(
                            "  MISMATCH {name} subtree {i}: our_root={} our_end={our_height} \
                             zebra_root={zebra_root} zebra_end={zebra_height}",
                            hex::encode(our_root),
                        );
                        mismatch = true;
                    }
                }
            }
        }
    }
    Ok(mismatch)
}

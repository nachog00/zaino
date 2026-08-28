#![forbid(unsafe_code)]
//! Source-port proficiency benchmark.
//!
//! The two things under test are implementations of the source port — the
//! abstraction over a validator. Drives the same hot-op suite (`compact` =
//! pre-index compact-block fetch, `tip` = chain-tip) through each source
//! (`ZebraRpcAdapter`, `ZebraReadStateAdapter`), wrapped in `ValidatorClient`,
//! on an identical basis. Sources run **in series** (never
//! concurrently) against one validator, sweeping concurrency across sampled
//! chain regions. Latency is measured harness-side with a single shared
//! instrumentation path so the two adapters are compared like for like.
//!
//! ReadState opens Zebra's RocksDB in-process as a read-only secondary (needs
//! the on-disk cache dir); RPC talks JSON-RPC to the same `zebrad`. Retry is
//! disabled (`max_attempts = 1`) so measured latency is the adapter's own, not
//! a retry ladder's.

mod report;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::Parser;
use futures::stream::{self, StreamExt};
use hdrhistogram::Histogram;
use tokio::task::JoinError;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

use zaino_primitives::types::{BlockHash, Height};
use zaino_rpc::{RpcClient, RpcClientConfig};
use zaino_source::{GetChainTip, GetPreIndexCompactBlock, RetryPolicy, ValidatorClient};
use zaino_source_zebra_readstate::ZebraReadStateAdapter;
use zaino_source_zebra_rpc::ZebraRpcAdapter;
use zebra_chain::parameters::Network;

use report::CellStats;

/// Compare Zaino source-port implementations (RPC vs ReadState) over a hot-op suite.
#[derive(Parser)]
#[command(name = "source-bench", about, version)]
struct Cli {
    /// Which adapter(s) to exercise: `both`, `readstate`, or `rpc`.
    #[arg(long, default_value = "both")]
    adapter: String,

    /// Zebra JSON-RPC URL for the RPC adapter.
    #[arg(
        long,
        default_value = "http://zebra.golden-zebra-state.svc.cluster.local:8232"
    )]
    rpc_url: String,

    /// On-disk Zebra state cache dir for the ReadState secondary (parent of `state/vN/<net>`).
    #[arg(long, default_value = "/zebra-cache")]
    cache_dir: PathBuf,

    /// First height of the `early` (small pre-sandblast blocks) region.
    #[arg(long, default_value_t = 10_000)]
    region_early_start: u32,

    /// First height of the `sandblast` (heavy shielded) region.
    #[arg(long, default_value_t = 1_700_000)]
    region_sandblast_start: u32,

    /// `recent` region = `tip - recent_offset .. tip`.
    #[arg(long, default_value_t = 2_000)]
    region_recent_offset: u32,

    /// Blocks sampled per region (contiguous from each region's start).
    #[arg(long, default_value_t = 1_000)]
    blocks_per_region: u32,

    /// Chain-tip calls issued per concurrency level.
    #[arg(long, default_value_t = 2_000)]
    tip_iters: u32,

    /// Comma-separated in-flight concurrency levels.
    #[arg(long, default_value = "1,2,4,8,16,32,64")]
    concurrency: String,

    /// Comma-separated ops to run: any of `compact`, `tip`.
    #[arg(long, default_value = "compact,tip")]
    ops: String,

    /// Heights to cross-check between adapters before benchmarking (both mode only).
    #[arg(long, default_value_t = 32)]
    parity_sample: usize,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    init_tracing();

    let want_readstate = matches!(cli.adapter.as_str(), "both" | "readstate");
    let want_rpc = matches!(cli.adapter.as_str(), "both" | "rpc");
    if !want_readstate && !want_rpc {
        anyhow::bail!("--adapter must be one of: both, readstate, rpc");
    }

    let concurrencies = parse_usize_csv(&cli.concurrency)?;
    let ops: Vec<String> = cli.ops.split(',').map(|s| s.trim().to_string()).collect();

    // No-retry policy: measure the adapter's own latency, not a retry ladder.
    let policy = RetryPolicy {
        max_attempts: 1,
        ..Default::default()
    };

    // Construct the selected adapters. ReadState opens the RocksDB secondary
    // (in-process); RPC builds an HTTP client.
    let rs_client = if want_readstate {
        let adapter = ZebraReadStateAdapter::open(&cli.cache_dir, &Network::Mainnet)
            .map_err(|e| anyhow::anyhow!("open ReadState secondary at {:?}: {e}", cli.cache_dir))?;
        info!(source = "readstate", cache_dir = ?cli.cache_dir, "opened readstate secondary");
        Some(Arc::new(ValidatorClient::new(adapter, policy.clone())))
    } else {
        None
    };

    let rpc_client = if want_rpc {
        let rpc = RpcClient::new(RpcClientConfig {
            url: cli.rpc_url.clone(),
            ..Default::default()
        })
        .context("build RPC client")?;
        info!(source = "rpc", url = %cli.rpc_url, "built rpc source");
        Some(Arc::new(ValidatorClient::new(
            ZebraRpcAdapter::new(rpc),
            policy,
        )))
    } else {
        None
    };

    // Pre-flight: chain tips. Use the minimum available tip so both sources
    // sample identical region heights even if the secondary lags the RPC node.
    let mut tips = Vec::new();
    if let Some(c) = &rs_client {
        match c.get_chain_tip().await {
            Ok((_, h)) => {
                let h = u32::from(h);
                info!(source = "readstate", height = h, "chain tip");
                tips.push(h);
            }
            Err(_) => warn!(source = "readstate", "get_chain_tip failed at startup"),
        }
    }
    if let Some(c) = &rpc_client {
        match c.get_chain_tip().await {
            Ok((_, h)) => {
                let h = u32::from(h);
                info!(source = "rpc", height = h, "chain tip");
                tips.push(h);
            }
            Err(_) => warn!(source = "rpc", "get_chain_tip failed at startup"),
        }
    }
    let tip = tips.iter().copied().min().unwrap_or(0);
    if tip == 0 {
        anyhow::bail!("could not determine chain tip from any source; aborting");
    }
    info!(tip, "using tip");

    // Build region height sets, clamped to the tip.
    let regions = build_regions(&cli, tip);
    for (name, heights) in &regions {
        let first = heights.first().map(|h| u32::from(*h));
        info!(region = %name, blocks = heights.len(), from = ?first, "region");
    }

    // Correctness gate: in both mode, confirm the sources agree before trusting timings.
    if let (Some(rs), Some(rp)) = (&rs_client, &rpc_client) {
        parity_check(rs, rp, &regions, cli.parity_sample).await;
    }

    // Series execution: fully exhaust one source before touching the next.
    let mut rows: Vec<CellStats> = Vec::new();
    if let Some(c) = &rs_client {
        info!(source = "readstate", "running sweeps");
        run_adapter(
            c,
            "readstate",
            &ops,
            &regions,
            cli.tip_iters,
            &concurrencies,
            &mut rows,
        )
        .await;
    }
    if let Some(c) = &rpc_client {
        info!(source = "rpc", "running sweeps");
        run_adapter(
            c,
            "rpc",
            &ops,
            &regions,
            cli.tip_iters,
            &concurrencies,
            &mut rows,
        )
        .await;
    }

    // Machine-readable results block for one-shot extraction from the logs,
    // alongside the per-cell structured events already emitted (Loki-queryable).
    let json = serde_json::to_string(&rows).context("serialize results")?;
    info!(cells = rows.len(), "benchmark complete");
    println!("=== SOURCE-BENCH-JSON-BEGIN ===");
    println!("{json}");
    println!("=== SOURCE-BENCH-JSON-END ===");

    Ok(())
}

/// Initialise JSON tracing to stdout so promtail → Loki → Grafana picks up each
/// event as structured fields. Override verbosity with `RUST_LOG`.
fn init_tracing() {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("source_bench=info"));
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(filter)
        .with_current_span(false)
        .with_span_list(false)
        .init();
}

/// Run every selected op × region × concurrency for one adapter, appending rows.
async fn run_adapter<V>(
    client: &Arc<ValidatorClient<V>>,
    adapter: &str,
    ops: &[String],
    regions: &[(String, Vec<Height>)],
    tip_iters: u32,
    concurrencies: &[usize],
    rows: &mut Vec<CellStats>,
) where
    V: Send + Sync + 'static,
    ValidatorClient<V>: GetPreIndexCompactBlock + GetChainTip,
{
    for op in ops {
        match op.as_str() {
            "compact" => {
                for (region_name, heights) in regions {
                    for &conc in concurrencies {
                        let cell = sweep_compact(client, adapter, region_name, heights, conc).await;
                        log_cell(&cell);
                        rows.push(cell);
                    }
                }
            }
            "tip" => {
                for &conc in concurrencies {
                    let cell = sweep_tip(client, adapter, tip_iters, conc).await;
                    log_cell(&cell);
                    rows.push(cell);
                }
            }
            other => warn!(op = other, "unknown op, skipping"),
        }
    }
}

/// Sweep compact-block fetch over `heights` at a fixed concurrency.
async fn sweep_compact<V>(
    client: &Arc<ValidatorClient<V>>,
    adapter: &str,
    region: &str,
    heights: &[Height],
    concurrency: usize,
) -> CellStats
where
    V: Send + Sync + 'static,
    ValidatorClient<V>: GetPreIndexCompactBlock,
{
    let start = Instant::now();
    let s = stream::iter(heights.iter().copied())
        .map(|height| {
            let c = client.clone();
            // spawn so calls land on worker threads — real parallelism for the
            // CPU-bound in-process ReadState reads as well as the IO-bound RPC.
            tokio::spawn(async move {
                let t = Instant::now();
                let ok = c.get_pre_index_compact_block(height).await.is_ok();
                (t.elapsed(), ok)
            })
        })
        .buffer_unordered(concurrency);
    let (hist, errors) = collect(Box::pin(s)).await;
    let wall = start.elapsed().as_secs_f64();
    CellStats::from_cell(adapter, "compact", region, concurrency, &hist, errors, wall)
}

/// Sweep chain-tip `iters` times at a fixed concurrency.
async fn sweep_tip<V>(
    client: &Arc<ValidatorClient<V>>,
    adapter: &str,
    iters: u32,
    concurrency: usize,
) -> CellStats
where
    V: Send + Sync + 'static,
    ValidatorClient<V>: GetChainTip,
{
    let start = Instant::now();
    let s = stream::iter(0..iters)
        .map(|_| {
            let c = client.clone();
            tokio::spawn(async move {
                let t = Instant::now();
                let ok = c.get_chain_tip().await.is_ok();
                (t.elapsed(), ok)
            })
        })
        .buffer_unordered(concurrency);
    let (hist, errors) = collect(Box::pin(s)).await;
    let wall = start.elapsed().as_secs_f64();
    CellStats::from_cell(adapter, "tip", "n/a", concurrency, &hist, errors, wall)
}

/// Drain a stream of spawned timed calls into a latency histogram + error count.
async fn collect<S>(mut s: S) -> (Histogram<u64>, u64)
where
    S: futures::Stream<Item = Result<(Duration, bool), JoinError>> + Unpin,
{
    let mut hist = Histogram::<u64>::new(3).expect("hdrhistogram sigfig 3 is in range");
    let mut errors = 0u64;
    while let Some(joined) = s.next().await {
        match joined {
            Ok((d, true)) => {
                let _ = hist.record(d.as_micros() as u64);
            }
            Ok((_, false)) => errors += 1,
            Err(_) => errors += 1, // task panic
        }
    }
    (hist, errors)
}

/// Cross-check that both adapters return the same block at sampled heights.
async fn parity_check<A, B>(
    rs: &Arc<ValidatorClient<A>>,
    rp: &Arc<ValidatorClient<B>>,
    regions: &[(String, Vec<Height>)],
    sample: usize,
) where
    A: Send + Sync + 'static,
    B: Send + Sync + 'static,
    ValidatorClient<A>: GetPreIndexCompactBlock,
    ValidatorClient<B>: GetPreIndexCompactBlock,
{
    // Sample evenly across all region heights.
    let all: Vec<Height> = regions
        .iter()
        .flat_map(|(_, h)| h.iter().copied())
        .collect();
    if all.is_empty() || sample == 0 {
        return;
    }
    let step = (all.len() / sample).max(1);
    let picks: Vec<Height> = all.iter().copied().step_by(step).take(sample).collect();

    let mut checked = 0usize;
    let mut mismatches = 0usize;
    for height in picks {
        let h = u32::from(height);
        let a = fetch_summary(rs, height).await;
        let b = fetch_summary(rp, height).await;
        match (a, b) {
            (Some(a), Some(b)) => {
                checked += 1;
                if a != b {
                    mismatches += 1;
                    warn!(height = h, readstate = ?a, rpc = ?b, "parity mismatch");
                }
            }
            _ => warn!(height = h, "block unavailable on one source"),
        }
    }
    if mismatches == 0 {
        info!(checked, mismatches, "parity: sources agree");
    } else {
        warn!(
            checked,
            mismatches, "parity: sources disagree — investigate"
        );
    }
}

/// `(hash, height, tx_count)` for a block via one source, or `None` on error.
async fn fetch_summary<V>(
    client: &Arc<ValidatorClient<V>>,
    height: Height,
) -> Option<(BlockHash, u32, usize)>
where
    V: Send + Sync + 'static,
    ValidatorClient<V>: GetPreIndexCompactBlock,
{
    match client.get_pre_index_compact_block(height).await {
        Ok(b) => Some((b.hash, b.height, b.transactions.len())),
        Err(_) => None,
    }
}

/// Build the three sampled regions, each clamped to `[start, min(start+n, tip)]`.
///
/// Heights are converted to `Height` once here: `tip` came from a valid
/// `get_chain_tip`, so every value in `start..=tip` is within the protocol
/// maximum. Any value that somehow overflows is dropped (not asserted), so the
/// hot path holds already-validated `Height`s.
fn build_regions(cli: &Cli, tip: u32) -> Vec<(String, Vec<Height>)> {
    let recent_start = tip.saturating_sub(cli.region_recent_offset);
    let specs = [
        ("early", cli.region_early_start),
        ("sandblast", cli.region_sandblast_start),
        ("recent", recent_start),
    ];
    let mut out = Vec::new();
    for (name, start) in specs {
        if start > tip {
            warn!(
                region = name,
                start, tip, "region start beyond tip, skipping"
            );
            continue;
        }
        let end = start.saturating_add(cli.blocks_per_region).min(tip + 1);
        let heights: Vec<Height> = (start..end)
            .filter_map(|h| Height::try_from(h).ok())
            .collect();
        if !heights.is_empty() {
            out.push((name.to_string(), heights));
        }
    }
    out
}

/// Parse a comma-separated list of positive concurrency levels.
fn parse_usize_csv(s: &str) -> Result<Vec<usize>> {
    s.split(',')
        .map(|t| {
            t.trim()
                .parse::<usize>()
                .with_context(|| format!("invalid concurrency '{t}'"))
                .and_then(|v| {
                    if v == 0 {
                        anyhow::bail!("concurrency must be > 0")
                    } else {
                        Ok(v)
                    }
                })
        })
        .collect()
}

/// Emit a completed cell as a structured event — the Loki-queryable result row.
fn log_cell(c: &CellStats) {
    info!(
        source = %c.adapter,
        op = %c.op,
        region = %c.region,
        concurrency = c.concurrency,
        count = c.count,
        errors = c.errors,
        throughput_ops_s = c.throughput_ops_s,
        p50_us = c.p50_us,
        p90_us = c.p90_us,
        p99_us = c.p99_us,
        max_us = c.max_us,
        "cell"
    );
}

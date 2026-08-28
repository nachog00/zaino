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

use std::collections::HashMap;
use std::sync::Mutex;

use anyhow::{Context, Result};
use clap::Parser;
use futures::stream::{self, StreamExt};
use hdrhistogram::Histogram;
use tokio::task::JoinError;
use tracing::field::{Field, Visit};
use tracing::{info, warn, Event, Subscriber};
use tracing_subscriber::filter::{EnvFilter, LevelFilter, Targets};
use tracing_subscriber::layer::Context as LayerContext;
use tracing_subscriber::prelude::*;
use tracing_subscriber::Layer;

use zaino_primitives::types::{BlockHash, Height, ShieldedPool, TransactionId};
use zaino_rpc::{RpcClient, RpcClientConfig};
use zaino_source::{
    GetChainTip, GetPreIndexCompactBlock, GetRawBlock, GetSubtreeRoots, GetTransaction,
    GetTreestate, RetryPolicy, ValidatorClient,
};
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

    /// Blocks sampled per region (contiguous from each region's start). Raise for
    /// a less noisy probe; drives the compact and treestate sweeps.
    #[arg(long, default_value_t = 2_000)]
    blocks_per_region: u32,

    /// Chain-tip calls issued per concurrency level.
    #[arg(long, default_value_t = 2_000)]
    tip_iters: u32,

    /// Transaction ids harvested + probed per region (transaction op).
    #[arg(long, default_value_t = 2_000)]
    tx_sample: usize,

    /// Subtree start indices probed per concurrency level (subtreeroots op).
    #[arg(long, default_value_t = 512)]
    subtree_count: u16,

    /// Shielded pool for the subtree probe: `sapling`, `orchard`, or `ironwood`.
    #[arg(long, default_value = "sapling")]
    subtree_pool: String,

    /// Zebra Prometheus metrics URL (port 8080). Scraped around each `rpcprofile`
    /// sweep to split the `prof_fetch` stage into network vs zebra-handler
    /// (`prof_zebra`). Empty to disable.
    #[arg(
        long,
        default_value = "http://zebra.golden-zebra-state.svc.cluster.local:8080/"
    )]
    zebra_metrics_url: String,

    /// Comma-separated in-flight concurrency levels.
    #[arg(long, default_value = "1,2,4,8,16,32,64")]
    concurrency: String,

    /// Comma-separated ops: any of `compact`, `rawblock`, `rpcprofile`, `tip`,
    /// `treestate`, `transaction`, `subtreeroots`. `rpcprofile` drives the real
    /// adapters' compact op and captures their instrumented stage events
    /// (`prof_fetch`/`prof_hex`/`prof_deserialize`/`prof_convert`/`prof_strip` for
    /// RPC; `prof_read`/`prof_convert`/`prof_strip` for ReadState).
    #[arg(
        long,
        default_value = "compact,rawblock,rpcprofile,tip,treestate,transaction,subtreeroots"
    )]
    ops: String,

    /// Heights to cross-check between sources before benchmarking (both mode only).
    #[arg(long, default_value_t = 32)]
    parity_sample: usize,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let stage_layer = init_tracing();

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

    // Build the shared workload once so both sources are driven over identical
    // inputs. The txid corpus is only harvested if the transaction op is selected.
    let tx_corpus = if ops.iter().any(|o| o == "transaction") {
        match (&rs_client, &rpc_client) {
            (Some(c), _) => collect_txids(c, &regions, cli.tx_sample).await,
            (None, Some(c)) => collect_txids(c, &regions, cli.tx_sample).await,
            (None, None) => Vec::new(),
        }
    } else {
        Vec::new()
    };
    let subtree_pool = parse_pool(&cli.subtree_pool)?;
    let subtree_indices: Vec<u16> = (0..cli.subtree_count).collect();
    let workload = Workload {
        regions,
        tx_corpus,
        subtree_indices,
        subtree_pool,
        tip_iters: cli.tip_iters,
    };

    // Series execution: fully exhaust one source before touching the next.
    let mut rows: Vec<CellStats> = Vec::new();
    if let Some(c) = &rs_client {
        info!(source = "readstate", "running sweeps");
        run_source(c, "readstate", &ops, &workload, &concurrencies, &mut rows).await;
    }
    if let Some(c) = &rpc_client {
        info!(source = "rpc", "running sweeps");
        run_source(c, "rpc", &ops, &workload, &concurrencies, &mut rows).await;
    }

    // Staged profile: drive the REAL adapters' compact op and capture their
    // instrumented `source::stage` events in-process (no emulation). RPC also
    // scrapes zebra's handler metric to split the `fetch` stage.
    if ops.iter().any(|o| o == "rpcprofile") {
        let http = reqwest::Client::builder()
            .build()
            .context("build zebra metrics http client")?;
        if let Some(c) = &rs_client {
            info!(source = "readstate", "running staged block profile");
            for (region, heights) in &workload.regions {
                for &conc in &concurrencies {
                    sweep_profile(
                        c,
                        "readstate",
                        &stage_layer,
                        region,
                        heights,
                        conc,
                        None,
                        &mut rows,
                    )
                    .await;
                }
            }
        }
        if let Some(c) = &rpc_client {
            let zebra = (!cli.zebra_metrics_url.is_empty())
                .then_some((&http, cli.zebra_metrics_url.as_str()));
            info!(source = "rpc", zebra = %cli.zebra_metrics_url, "running staged block profile");
            for (region, heights) in &workload.regions {
                for &conc in &concurrencies {
                    sweep_profile(
                        c,
                        "rpc",
                        &stage_layer,
                        region,
                        heights,
                        conc,
                        zebra,
                        &mut rows,
                    )
                    .await;
                }
            }
        }
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

/// Initialise tracing and return the stage-capture layer handle.
///
/// Two composed layers, each with its own filter: a JSON fmt layer (our `info`
/// logs → stdout → Loki/Grafana), and a [`StageLayer`] that captures the
/// adapters' `source::stage` TRACE events in-process for profiling. The fmt
/// filter excludes the stage events so they don't flood the logs.
fn init_tracing() -> StageLayer {
    let fmt_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("source_bench=info"));
    let fmt_layer = tracing_subscriber::fmt::layer()
        .json()
        .with_current_span(false)
        .with_span_list(false)
        .with_filter(fmt_filter);

    let stage_layer = StageLayer::default();
    let handle = stage_layer.clone();
    let stage_filter = Targets::new().with_target("source::stage", LevelFilter::TRACE);

    tracing_subscriber::registry()
        .with(fmt_layer)
        .with(stage_layer.with_filter(stage_filter))
        .init();
    handle
}

/// Captures the adapters' `source::stage` events (each carrying `stage` and
/// `micros` fields) into a histogram per stage name. Reset before a profile
/// sweep and drained after, so the histograms hold exactly that sweep's stages.
#[derive(Clone, Default)]
struct StageLayer {
    hists: std::sync::Arc<Mutex<HashMap<String, Histogram<u64>>>>,
}

impl StageLayer {
    fn reset(&self) {
        if let Ok(mut h) = self.hists.lock() {
            h.clear();
        }
    }

    fn take(&self) -> HashMap<String, Histogram<u64>> {
        match self.hists.lock() {
            Ok(mut h) => std::mem::take(&mut *h),
            Err(_) => HashMap::new(),
        }
    }
}

/// Field visitor pulling `stage` (str) and `micros` (u64) off a stage event.
#[derive(Default)]
struct StageVisit {
    stage: Option<String>,
    micros: Option<u64>,
}

impl Visit for StageVisit {
    fn record_u64(&mut self, field: &Field, value: u64) {
        if field.name() == "micros" {
            self.micros = Some(value);
        }
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "stage" {
            self.stage = Some(value.to_string());
        }
    }
    fn record_debug(&mut self, _field: &Field, _value: &dyn std::fmt::Debug) {}
}

impl<S: Subscriber> Layer<S> for StageLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: LayerContext<'_, S>) {
        let mut v = StageVisit::default();
        event.record(&mut v);
        if let (Some(stage), Some(micros)) = (v.stage, v.micros) {
            if let Ok(mut map) = self.hists.lock() {
                let hist = map.entry(stage).or_insert_with(|| {
                    Histogram::<u64>::new(3).expect("hdrhistogram sigfig 3 is in range")
                });
                let _ = hist.record(micros);
            }
        }
    }
}

/// Everything a source needs to be swept, built once and shared by both sources
/// so the two are driven over identical inputs.
struct Workload {
    /// Region name → block heights (compact, treestate).
    regions: Vec<(String, Vec<Height>)>,
    /// Region name → harvested transaction ids (transaction).
    tx_corpus: Vec<(String, Vec<TransactionId>)>,
    /// Subtree start indices to probe (subtreeroots).
    subtree_indices: Vec<u16>,
    /// Pool the subtree probe targets.
    subtree_pool: ShieldedPool,
    /// Chain-tip calls per concurrency level.
    tip_iters: u32,
}

/// Run every selected op × region × concurrency for one source, appending rows.
async fn run_source<V>(
    client: &Arc<ValidatorClient<V>>,
    source: &str,
    ops: &[String],
    workload: &Workload,
    concurrencies: &[usize],
    rows: &mut Vec<CellStats>,
) where
    V: Send + Sync + 'static,
    ValidatorClient<V>: GetPreIndexCompactBlock
        + GetRawBlock
        + GetChainTip
        + GetTreestate
        + GetTransaction
        + GetSubtreeRoots,
{
    for op in ops {
        match op.as_str() {
            "compact" => {
                for (region, heights) in &workload.regions {
                    for &conc in concurrencies {
                        let (h, e, w) = drive(heights.clone(), conc, |height| {
                            let c = client.clone();
                            async move { c.get_pre_index_compact_block(height).await.is_ok() }
                        })
                        .await;
                        push_cell(rows, source, "compact", region, conc, &h, e, w);
                    }
                }
            }
            // Same fetch as `compact` up to consensus-deserialize: RPC stops at
            // hex-decode, ReadState reads typed then re-serializes. So on the RPC
            // side, compact − rawblock isolates zaino's consensus-deserialize cost.
            "rawblock" => {
                for (region, heights) in &workload.regions {
                    for &conc in concurrencies {
                        let (h, e, w) = drive(heights.clone(), conc, |height| {
                            let c = client.clone();
                            async move { c.get_raw_block(height).await.is_ok() }
                        })
                        .await;
                        push_cell(rows, source, "rawblock", region, conc, &h, e, w);
                    }
                }
            }
            "treestate" => {
                for (region, heights) in &workload.regions {
                    for &conc in concurrencies {
                        let (h, e, w) = drive(heights.clone(), conc, |height| {
                            let c = client.clone();
                            async move { c.get_treestate(height).await.is_ok() }
                        })
                        .await;
                        push_cell(rows, source, "treestate", region, conc, &h, e, w);
                    }
                }
            }
            "transaction" => {
                for (region, txids) in &workload.tx_corpus {
                    if txids.is_empty() {
                        warn!(region = %region, "no txids harvested, skipping transaction sweep");
                        continue;
                    }
                    for &conc in concurrencies {
                        let (h, e, w) = drive(txids.clone(), conc, |txid| {
                            let c = client.clone();
                            async move { c.get_transaction(txid).await.is_ok() }
                        })
                        .await;
                        push_cell(rows, source, "transaction", region, conc, &h, e, w);
                    }
                }
            }
            "subtreeroots" => {
                let pool = workload.subtree_pool;
                let region = pool_label(pool);
                for &conc in concurrencies {
                    let (h, e, w) = drive(workload.subtree_indices.clone(), conc, |idx| {
                        let c = client.clone();
                        async move { c.get_subtree_roots(pool, idx, Some(1)).await.is_ok() }
                    })
                    .await;
                    push_cell(rows, source, "subtreeroots", region, conc, &h, e, w);
                }
            }
            "tip" => {
                for &conc in concurrencies {
                    let items: Vec<u32> = (0..workload.tip_iters).collect();
                    let (h, e, w) = drive(items, conc, |_| {
                        let c = client.clone();
                        async move { c.get_chain_tip().await.is_ok() }
                    })
                    .await;
                    push_cell(rows, source, "tip", "n/a", conc, &h, e, w);
                }
            }
            // RPC-only staged profile — handled separately in main, not here.
            "rpcprofile" => {}
            other => warn!(op = other, "unknown op, skipping"),
        }
    }
}

/// Profile the REAL adapter's compact-block path: drive
/// `get_pre_index_compact_block` over `heights` while the [`StageLayer`]
/// captures the adapter's instrumented `source::stage` events, then emit one
/// cell per stage (`prof_<stage>`). No emulation — the numbers come straight
/// from the adapter. For RPC, also scrape zebra's handler metric to split the
/// `fetch` stage into zebra-handler vs network (`prof_zebra`).
#[allow(clippy::too_many_arguments)]
async fn sweep_profile<V>(
    client: &Arc<ValidatorClient<V>>,
    source: &str,
    layer: &StageLayer,
    region: &str,
    heights: &[Height],
    concurrency: usize,
    zebra: Option<(&reqwest::Client, &str)>,
    rows: &mut Vec<CellStats>,
) where
    V: Send + Sync + 'static,
    ValidatorClient<V>: GetPreIndexCompactBlock,
{
    // Snapshot zebra's cumulative getblock handler (sum,count) before the sweep;
    // the delta across the sweep is the windowed mean handler time for exactly
    // these calls (the target validator's getblock counter is unshared).
    let before = match zebra {
        Some((c, u)) => scrape_zebra_getblock(c, u).await,
        None => None,
    };

    layer.reset();
    let (_total, errors, wall) = drive(heights.to_vec(), concurrency, |height| {
        let c = client.clone();
        async move { c.get_pre_index_compact_block(height).await.is_ok() }
    })
    .await;

    // Per-stage histograms captured from the adapter during the sweep.
    for (stage, hist) in &layer.take() {
        push_cell(
            rows,
            source,
            &format!("prof_{stage}"),
            region,
            concurrency,
            hist,
            errors,
            wall,
        );
    }

    // zebra-handler mean over exactly this sweep's getblock calls; splits the
    // `fetch` stage into zebra-handler (this) + network (the remainder). Compared
    // mean-to-mean against the `fetch` stage in the report.
    if let (Some((c, u)), Some((s0, n0))) = (zebra, before) {
        if let Some((s1, n1)) = scrape_zebra_getblock(c, u).await {
            if n1 > n0 {
                let mean_us = ((s1 - s0) / ((n1 - n0) as f64) * 1e6) as u64;
                let mut hz = Histogram::<u64>::new(3).expect("hdrhistogram sigfig 3 is in range");
                let _ = hz.record(mean_us);
                push_cell(
                    rows,
                    source,
                    "prof_zebra",
                    region,
                    concurrency,
                    &hz,
                    0,
                    wall,
                );
            }
        }
    }
}

/// Scrape zebra's `/metrics` and return the cumulative
/// `rpc_request_duration_seconds_{sum,count}` for `method="getblock"`.
async fn scrape_zebra_getblock(client: &reqwest::Client, url: &str) -> Option<(f64, u64)> {
    let body = client.get(url).send().await.ok()?.text().await.ok()?;
    let mut sum = None;
    let mut count = None;
    for line in body.lines() {
        if let Some(rest) =
            line.strip_prefix("rpc_request_duration_seconds_sum{method=\"getblock\"}")
        {
            sum = rest.trim().parse::<f64>().ok();
        } else if let Some(rest) =
            line.strip_prefix("rpc_request_duration_seconds_count{method=\"getblock\"}")
        {
            count = rest.trim().parse::<f64>().ok().map(|f| f as u64);
        }
    }
    Some((sum?, count?))
}

/// Build a cell record from a completed sweep, log it, and append it.
#[allow(clippy::too_many_arguments)]
fn push_cell(
    rows: &mut Vec<CellStats>,
    source: &str,
    op: &str,
    region: &str,
    concurrency: usize,
    hist: &Histogram<u64>,
    errors: u64,
    wall_secs: f64,
) {
    let cell = CellStats::from_cell(source, op, region, concurrency, hist, errors, wall_secs);
    log_cell(&cell);
    rows.push(cell);
}

/// Human label for a shielded pool.
fn pool_label(pool: ShieldedPool) -> &'static str {
    match pool {
        ShieldedPool::Sapling => "sapling",
        ShieldedPool::Orchard => "orchard",
        ShieldedPool::Ironwood => "ironwood",
    }
}

/// Parse the `--subtree-pool` flag.
fn parse_pool(s: &str) -> Result<ShieldedPool> {
    match s.to_ascii_lowercase().as_str() {
        "sapling" => Ok(ShieldedPool::Sapling),
        "orchard" => Ok(ShieldedPool::Orchard),
        "ironwood" => Ok(ShieldedPool::Ironwood),
        other => anyhow::bail!("unknown pool '{other}' (sapling|orchard|ironwood)"),
    }
}

/// Drive `items` through `make` at a fixed in-flight `concurrency`, timing each
/// call. Each item is spawned so calls land on worker threads — real parallelism
/// for the CPU-bound in-process ReadState reads as well as the IO-bound RPC.
/// Returns the latency histogram, error count, and wall-clock seconds.
async fn drive<T, Fut, F>(items: Vec<T>, concurrency: usize, make: F) -> (Histogram<u64>, u64, f64)
where
    T: Send + 'static,
    F: Fn(T) -> Fut,
    Fut: std::future::Future<Output = bool> + Send + 'static,
{
    let start = Instant::now();
    let s = stream::iter(items)
        .map(|item| {
            let fut = make(item);
            tokio::spawn(async move {
                let t = Instant::now();
                let ok = fut.await;
                (t.elapsed(), ok)
            })
        })
        .buffer_unordered(concurrency);
    let (hist, errors) = collect(Box::pin(s)).await;
    (hist, errors, start.elapsed().as_secs_f64())
}

/// Harvest up to `cap` transaction ids per region by scanning that region's
/// blocks in order until the cap is met. Built once from one source and reused
/// for both, so the transaction sweep drives an identical txid set on each.
async fn collect_txids<V>(
    client: &Arc<ValidatorClient<V>>,
    regions: &[(String, Vec<Height>)],
    cap: usize,
) -> Vec<(String, Vec<TransactionId>)>
where
    V: Send + Sync + 'static,
    ValidatorClient<V>: GetPreIndexCompactBlock,
{
    let mut out = Vec::new();
    for (region, heights) in regions {
        let mut txids = Vec::new();
        for &height in heights {
            if txids.len() >= cap {
                break;
            }
            if let Ok(block) = client.get_pre_index_compact_block(height).await {
                for tx in block.transactions {
                    txids.push(tx.txid);
                    if txids.len() >= cap {
                        break;
                    }
                }
            }
        }
        info!(region = %region, txids = txids.len(), "harvested tx corpus");
        out.push((region.clone(), txids));
    }
    out
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

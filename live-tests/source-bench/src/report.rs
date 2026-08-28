//! Result record for one benchmark cell and human/JSON rendering.
//!
//! A cell is one `(adapter, op, region, concurrency)` combination. Each cell
//! measures per-call wall-clock latency with a single shared instrumentation
//! path, so the two adapters are compared on an identical basis.

use hdrhistogram::Histogram;
use serde::Serialize;

/// Measured results for a single `(adapter, op, region, concurrency)` cell.
#[derive(Serialize, Clone)]
pub(crate) struct CellStats {
    /// `readstate` or `rpc`.
    pub(crate) adapter: String,
    /// `compact` (compact-block fetch) or `tip` (chain-tip).
    pub(crate) op: String,
    /// Sampled chain region (`early` / `sandblast` / `recent`), or `n/a` for tip.
    pub(crate) region: String,
    /// In-flight request cap during this cell.
    pub(crate) concurrency: usize,
    /// Successful calls recorded.
    pub(crate) count: u64,
    /// Calls that returned an error or whose task panicked.
    pub(crate) errors: u64,
    /// Wall-clock time for the whole cell.
    pub(crate) wall_ms: f64,
    /// Successful calls per second over the wall-clock window.
    pub(crate) throughput_ops_s: f64,
    /// Per-call latency percentiles, microseconds.
    pub(crate) p50_us: u64,
    pub(crate) p90_us: u64,
    pub(crate) p99_us: u64,
    pub(crate) max_us: u64,
}

impl CellStats {
    /// Build a record from a completed cell's histogram and counters.
    pub(crate) fn from_cell(
        adapter: &str,
        op: &str,
        region: &str,
        concurrency: usize,
        hist: &Histogram<u64>,
        errors: u64,
        wall_secs: f64,
    ) -> Self {
        let count = hist.len();
        let throughput_ops_s = if wall_secs > 0.0 {
            count as f64 / wall_secs
        } else {
            0.0
        };
        Self {
            adapter: adapter.to_string(),
            op: op.to_string(),
            region: region.to_string(),
            concurrency,
            count,
            errors,
            wall_ms: wall_secs * 1000.0,
            throughput_ops_s,
            p50_us: hist.value_at_quantile(0.50),
            p90_us: hist.value_at_quantile(0.90),
            p99_us: hist.value_at_quantile(0.99),
            max_us: hist.max(),
        }
    }
}

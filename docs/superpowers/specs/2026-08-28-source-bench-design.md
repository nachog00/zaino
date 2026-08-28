# Adapter proficiency benchmark — design

Date: 2026-08-28
Base branch: `feat/resilient-seam` (worktree `source-bench`, branch `feat/source-bench`)

## Goal

Answer, empirically and on a fair basis: **for the operations the indexer actually
needs, is one Zebra source adapter faster than the other, by how much, and where does
each saturate?** The two adapters under test are the source-seam ports over:

- `ValidatorClient<ZebraRpcAdapter>` — Zcash JSON-RPC over HTTP to a `zebrad` process.
- `ValidatorClient<ZebraReadStateAdapter>` — Zebra `zebra-state` opened in-process as a
  read-only RocksDB **secondary** (no network; deserialize happens inside the harness).

The north star is a fully-integrated A/B where the indexer itself runs in each mode.
This experiment is the **initial proxy**: the same requests, driven at the bare adapter
seam, on an identical basis, so transport-vs-in-process is the only variable.

## Two lenses (built in phases)

1. **Differential latency/throughput (Phase 1 — this spec).** The hot-op suite driven
   through each adapter, symmetric harness-side timing (hdrhistogram), swept over
   concurrency and chain region. Answers *is it really slower, by how much, where does
   the curve break*. Buildable today; no observability plumbing.
2. **Attribution (Phase 2 — deferred).** *Why* — is it the serde/wire round-trip.
   See "Phase 2 notes" for the verified capability surface.

## Phase 1 scope (approved)

- **Operations (all in the both-sources intersection):** `GetPreIndexCompactBlock`
  (compact-block fetch — what sync consumes; note neither source has a true compact read,
  both fetch whole blocks and strip, which is what the indexer pays anyway), `GetChainTip`,
  `GetTreestate` and `GetTransaction` (both region-indexed — treestate by height,
  transaction over a txid corpus harvested once from the compact blocks and reused for
  both sources), and `GetSubtreeRoots` (indexed by subtree start-index over a shielded
  pool, its own axis — not region-based).
- **Chain coverage:** three fixed sampled regions rather than the full ~2.9M sweep —
  early/small blocks, sandblast-heavy (~1.7–1.9M), recent-near-tip. Every probe size is a
  CLI flag (`--blocks-per-region`, `--tip-iters`, `--tx-sample`, `--subtree-count`);
  defaults sized to run in minutes, raise for a less noisy probe.
- **Concurrency sweep:** 1, 2, 4, 8, 16, 32, 64 (loadgen found saturation ~64).
- **Discipline:** adapters run **in series**, never concurrently — no cross-contamination.
- **Measurement:** harness-side wall-clock only, identical instrumentation on both
  adapters (sidesteps the metric asymmetry — the RPC path has outbound metrics upstream,
  the ReadState path does not).

## Architecture

New crate `live-tests/source-bench` (binary + `Containerfile`), added to workspace
`members` (not `default-members`, matching the live-test convention so normal builds
skip it). Named for what's under test — the **source-port implementations** over the
validator — not the incidental adapter role.

- **Config/CLI:** adapter selection (`rpc` | `readstate` | `both`), op set, region
  definitions (start height + count per region), concurrency levels, RPC URL, RocksDB
  cache dir, network (mainnet).
- **Driver:** for each `adapter → op → concurrency` cell, issue the request set against
  sampled heights, timing each call; a bounded worker pool provides concurrency; collect
  per-cell hdrhistogram → throughput + p50/p90/p99/max.
- **Output:** human-readable table to stdout + structured JSON to a file/stdout for
  later charting. No Prometheus in Phase 1.
- **Parity check (smoke value):** for a sample of heights, assert both adapters return
  equal compact-block bytes/hash, so we know we're comparing like for like before
  trusting timings.

## Deploy

- **Must run in-cluster on `tekau`**: ReadState's RocksDB secondary needs the on-disk DB
  files, which live only on that node's hostPath. Off-node execution is impossible for
  the ReadState half.
- **Target validator:** `golden-zebra-state` (dedicated, at tip). Mount its RocksDB
  hostPath (`/srv/zebra-state-cache-mainnet`) read-only for ReadState; reach
  `zebra.golden-zebra-state.svc:8232` for RPC. One zebra, same data, series runs → zero
  contention.
- **Build/run:** in-cluster BuildKit builds `live-tests/adapter-bench/Containerfile` from
  the pushed `feat/adapter-bench` ref (mirrors `bench-zaino.yaml`); a k8s Job pinned to
  `tekau` runs it and prints results to pod logs. A dedicated Argo `WorkflowTemplate`
  (`bench-adapters`) wraps build + run.

## Phase 2 notes (deferred — verified capability surface)

- Stock `zebrad:6.3.0` already compiles in **Prometheus** (per-method
  `rpc.request.duration_seconds` histogram — real RPC-handler latency) and **OTLP/HTTP
  span export to Tempo (:4318)**; both need only config. This gives a cheap decomposition:
  `harness total = zebra RPC-handler + network + zaino parse`, with the handler term read
  straight from zebra.
- Zebra does **not** propagate inbound W3C trace-context → zaino and zebra traces are
  separate roots (correlate by time + `rpc.method`).
- No per-*read* state-latency histogram in zebra (only counters + a write-commit
  histogram). CPU flamegraphs need a custom zebra rebuild and are span-based
  (`tracing-flame`), not perf CPU sampling; tokio-console likewise needs a special build.
- Under-traced paths already mapped (for a coverage pass): the whole `zaino-fetch` RPC
  connector + response parsers, the inbound `zaino-serve` layer, and the StateService
  read path + encode/decode hot path.

## Risks

- **RocksDB secondary version/format compat:** the seam links `zebra-state = "13.0"`; the
  cluster runs `zebra 6.3.0`. The on-disk column-family format must match for
  `init_read_only` to open the secondary. First run surfaces any mismatch immediately;
  if it fails, fall back to running ReadState against a zebra whose `zebra-state` version
  matches, or pin the adapter's `zebra-state` to the cluster's.
- **Secondary staleness:** a RO secondary sees the primary's state as of last catch-up;
  fine for historical-region reads, and `GetChainTip` tolerance is acceptable for a bench.

# zaino-wallet

The full-wallet library adapter: a stable-DTO facade over `FullWalletService`
for a wallet that embeds Zaino in-process (zallet). This is the driving-adapter
half of the hexagon — a public library *is* a driving adapter with its own
stable types — not a port: it defines no interface for others to implement.

## Shape: `Indexer` + `WalletSnapshot`

Mirrors zallet's own `Chain`/`ChainView` split (see `live-tests/zallet-fit` on
`feat/driving-port-scaffold` for the mirrored trait this shape was checked
against):

- **`Indexer<W: FullWalletService>`** — the engine handle. Holds what is not
  pinned to one chain position: `broadcast`, `reported_upgrades`,
  `subscribe_tip`, `subscribe_mempool`.
- **`Indexer::snapshot()`** — pins the current view into a **`WalletSnapshot`**,
  which carries every read that must stay coherent against one tip: compact
  blocks, treestate/subtree roots, address history/UTXOs, raw transaction and
  block bytes, transaction status.

A wallet scanning a batch of blocks, then checking an address's balance,
wants both answered against the same chain position — re-pinning per call
(as the crate did before this split) could see the tip move between them.

## No internal type crosses the boundary

Every read returns a type from `dto`, never a `zaino_primitives` or
`zaino_service` domain type directly, so the inner primitives can evolve
without breaking the embedded consumer. Conversions are one method per
direction, named for it: `from_domain` for a read coming out, `into_domain`
for a caller-supplied selector going in (`WalletBlockSelector`,
`WalletShieldedPool`). This is the mistake the crate's design prevents: a
one-line convenience `impl From<Height> for u32` creep that would let a
primitives-crate rename or type change break zallet's build with no seam to
catch it.

`WalletCompactBlock` is the one deep DTO tree (mirroring the pool-decomposed
`CompactBlock`/`PreIndexCompactTx` field-for-field in `dto::compact_block`):
a wallet's note scanner needs the real nullifiers/commitments/ephemeral keys,
not an opaque blob. Every other read either converts a flat domain struct
one-to-one, or — `raw_transaction`/`raw_block` — passes the bytes straight
through, matching `RawTransactionRead`'s own precedent that a wallet parses
transactions (and, since `RawBlockRead`, blocks) itself.

## Bound to `FullWalletService` alone

`Indexer`/`WalletSnapshot` depend on exactly the capabilities the full-wallet
use case needs (`FullWalletReads = WalletReadCore + RawBlockRead +
TransactionRead`, plus the engine controls), never the wider inner surface —
so adding a node-rpc-only capability to the engine cannot accidentally widen
what this crate exposes.

## Try it

`examples/full_wallet_demo.rs` is a runnable, narrated walk through the whole
public surface — every `Indexer`/`WalletSnapshot` method, each DTO printed and
asserted against a scripted `MockIndexerService` (there is no real
`FullWalletService` deployment wired up anywhere yet, so this is the adapter's
wiring proven, not a real engine's answers):

```text
cargo run -p zaino-wallet --example full_wallet_demo
```

## What is deliberately not here yet

`ForkReconcile` (zallet's `find_fork_point` + raw-block-since-fork replay) has
no implementation anywhere in the engine yet — see
`zaino-service::ForkReconcile`'s doc and the `fullwallet_zallet_gap` design
note. Wiring it into this crate is separate follow-up work once the engine
side lands.

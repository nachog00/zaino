//! Runnable, narrated walk through `zaino-wallet`'s whole public surface —
//! `Indexer` and `WalletSnapshot`, every read and control, each DTO the
//! boundary actually crosses.
//!
//! Run it with:
//!
//! ```text
//! cargo run -p zaino-wallet --example full_wallet_demo --features testing
//! ```
//!
//! **Honest about what this is and isn't.** There is no real `FullWalletService`
//! deployment wired up anywhere on this branch yet (see the `fullwallet_zallet_gap`
//! design note) — no zebra, no zainod, no sync engine. This runs against
//! `zaino-service::testing::MockIndexerService`, a hand-scripted stand-in for
//! *any* engine, pre-loaded with fixed answers below. It proves the adapter's
//! wiring and DTO conversions are correct — exactly what this crate's own test
//! suite proves — not that a real engine answers correctly under real chain
//! conditions (that's `zaino-core`'s job, and its own test suite's claim).
//!
//! Every step prints what it asked for and what came back, then asserts the
//! scripted value survived the round trip, so a regression fails this example
//! loudly rather than silently printing a wrong number.

use futures::StreamExt;

use zaino_primitives::types::{
    AddressBalance, AddressDelta, BlockHash, BlockRef, Height, PoolTreestate, RawTransaction,
    Script, SubtreeRoot, TransactionId, TransactionLocation, TransparentAddress, TreeRoot,
    Treestate, Utxo, Zatoshis, ZatoshisFlowSum,
};
use zaino_service::testing::{MockChain, MockIndexerService};
use zaino_service::MempoolTx;
use zaino_wallet::{
    Indexer, WalletAddressBalance, WalletAddressDelta, WalletBlockSelector, WalletLocatedTxid,
    WalletPoolTreestate, WalletRawTransaction, WalletShieldedPool, WalletSubtreeRoot, WalletTip,
    WalletTransactionLocation, WalletTreestate, WalletTxStatus, WalletUtxo,
};

const ADDR: &str = "t1exampleaddress000000000000000000";

#[tokio::main(flavor = "current_thread")]
async fn main() {
    println!("=== zaino-wallet demo: scripting a mock chain ===\n");

    let tip = BlockRef {
        height: Height::try_from(1_000).expect("valid height"),
        hash: BlockHash::from([0x42; 32]),
    };
    let chain = MockChain {
        tip: Some(tip),
        balances: vec![(
            ADDR.to_owned(),
            AddressBalance {
                balance: Zatoshis::new(12_345_000).expect("valid amount"),
                received: ZatoshisFlowSum::from_summed(50_000_000),
            },
        )],
        utxos: vec![Utxo {
            address: TransparentAddress::new(ADDR.to_owned()),
            txid: TransactionId::from([0x11; 32]),
            output_index: 0,
            script: Script::new(vec![0x76, 0xa9, 0x14]),
            satoshis: Zatoshis::new(5_000_000).expect("valid amount"),
            height: Height::try_from(900).expect("valid height"),
        }],
        deltas: vec![AddressDelta {
            satoshis: zaino_primitives::types::SignedZatoshis::try_new(5_000_000)
                .expect("valid amount"),
            txid: TransactionId::from([0x11; 32]),
            index: 0,
            height: Height::try_from(900).expect("valid height"),
            address: TransparentAddress::new(ADDR.to_owned()),
            block_index: Some(2),
        }],
        txids: vec![TransactionId::from([0x11; 32])],
        raw_transactions: vec![(
            TransactionId::from([0x11; 32]),
            RawTransaction {
                data: vec![0xde, 0xad, 0xbe, 0xef],
                location: TransactionLocation::BestChain(
                    Height::try_from(900).expect("valid height"),
                ),
            },
        )],
        raw_block: Some(vec![0x00, 0x01, 0x02, 0x03]),
        treestate: Some(Treestate {
            block_hash: tip.hash,
            height: tip.height,
            time: 1_760_000_000,
            sapling: Some(PoolTreestate {
                final_root: Some(TreeRoot::new([0x55; 32])),
                final_state: vec![0xaa, 0xbb],
            }),
            orchard: None,
            ironwood: None,
        }),
        subtree_roots: vec![SubtreeRoot {
            root: TreeRoot::new([0x66; 32]),
            completing_block_hash: BlockHash::from([0x77; 32]),
            end_height: Height::try_from(512).expect("valid height"),
        }],
        mempool: vec![MempoolTx {
            txid: TransactionId::from([0x22; 32]),
            validated_against: tip,
        }],
        ..Default::default()
    };

    let indexer = Indexer::new(MockIndexerService::new(chain));
    println!("Indexer built over MockIndexerService.\n");

    // --- Engine-level controls (Indexer) --------------------------------

    println!("--- Indexer: engine-level controls ---");

    let upgrades = indexer
        .reported_upgrades()
        .await
        .expect("reported_upgrades");
    println!("reported_upgrades() -> {upgrades:?} (mock reports none)");
    assert!(upgrades.is_empty());

    let txid = indexer
        .broadcast(vec![0x01, 0x02, 0x03])
        .await
        .expect("broadcast");
    println!("broadcast(..) -> {txid:?}");

    let tip_event = indexer
        .subscribe_tip()
        .next()
        .await
        .expect("current tip event");
    println!("subscribe_tip() first event -> {tip_event:?}");
    assert_eq!(
        tip_event.tip,
        WalletTip {
            height: 1_000,
            hash: [0x42; 32],
        }
    );

    let mempool_event = indexer
        .subscribe_mempool()
        .next()
        .await
        .expect("scripted mempool entry");
    println!("subscribe_mempool() first event -> {mempool_event:?}\n");
    assert_eq!(mempool_event.txid.0, [0x22; 32]);

    // --- Pinned reads (WalletSnapshot) -----------------------------------

    println!("--- Indexer::snapshot() -> WalletSnapshot: pinned reads ---");
    let snapshot = indexer.snapshot().await.expect("a coherent snapshot");

    let snapshot_tip = snapshot.tip().expect("the chain has a tip");
    println!("tip() -> {snapshot_tip:?}");
    assert_eq!(
        snapshot_tip,
        WalletTip {
            height: 1_000,
            hash: [0x42; 32],
        }
    );

    let balance = snapshot
        .address_balance(ADDR, 0, 1_000)
        .await
        .expect("address_balance");
    println!("address_balance({ADDR:?}, 0..=1000) -> {balance:?}");
    assert_eq!(
        balance,
        WalletAddressBalance {
            balance: 12_345_000,
            received: 50_000_000,
        }
    );

    let utxos = snapshot.address_utxos(ADDR).await.expect("address_utxos");
    println!("address_utxos({ADDR:?}) -> {utxos:?}");
    assert_eq!(
        utxos,
        vec![WalletUtxo {
            address: ADDR.to_owned(),
            txid: [0x11; 32],
            output_index: 0,
            script: vec![0x76, 0xa9, 0x14],
            satoshis: 5_000_000,
            height: 900,
        }]
    );

    let deltas = snapshot
        .address_deltas(ADDR, 0, 1_000)
        .await
        .expect("address_deltas");
    println!("address_deltas({ADDR:?}, 0..=1000) -> {deltas:?}");
    assert_eq!(
        deltas,
        vec![WalletAddressDelta {
            satoshis: 5_000_000,
            txid: [0x11; 32],
            index: 0,
            height: 900,
            address: ADDR.to_owned(),
            block_index: Some(2),
        }]
    );

    let tx_ids = snapshot
        .address_tx_ids(ADDR, 0, 1_000)
        .await
        .expect("address_tx_ids");
    println!("address_tx_ids({ADDR:?}, 0..=1000) -> {tx_ids:?}");
    assert_eq!(
        tx_ids,
        vec![WalletLocatedTxid {
            height: Some(0),
            block_index: None,
            txid: [0x11; 32],
        }]
    );

    let raw_tx = snapshot
        .raw_transaction([0x11; 32])
        .await
        .expect("raw_transaction read")
        .expect("the scripted transaction");
    println!("raw_transaction([0x11; 32]) -> {raw_tx:?}");
    assert_eq!(
        raw_tx,
        WalletRawTransaction {
            data: vec![0xde, 0xad, 0xbe, 0xef],
            location: WalletTransactionLocation::BestChain(900),
        }
    );

    let status = snapshot
        .transaction_status([0x11; 32])
        .await
        .expect("transaction_status read");
    println!("transaction_status([0x11; 32]) -> {status:?} (the mock always answers Unknown)");
    assert_eq!(status, WalletTxStatus::Unknown);

    let raw_block = snapshot
        .raw_block(WalletBlockSelector::Height(1_000))
        .await
        .expect("raw_block read")
        .expect("the scripted block");
    println!("raw_block(Height(1000)) -> {raw_block:?}");
    assert_eq!(raw_block, vec![0x00, 0x01, 0x02, 0x03]);

    let treestate = snapshot.treestate(1_000).await.expect("treestate read");
    println!("treestate(1000) -> {treestate:?}");
    assert_eq!(
        treestate,
        WalletTreestate {
            block_hash: [0x42; 32],
            height: 1_000,
            time: 1_760_000_000,
            sapling: Some(WalletPoolTreestate {
                final_root: Some([0x55; 32]),
                final_state: vec![0xaa, 0xbb],
            }),
            orchard: None,
            ironwood: None,
        }
    );

    let roots = snapshot
        .subtree_roots(WalletShieldedPool::Sapling, 0, None)
        .await
        .expect("subtree_roots read");
    println!("subtree_roots(Sapling, 0, None) -> {roots:?}");
    assert_eq!(
        roots,
        vec![WalletSubtreeRoot {
            root: [0x66; 32],
            completing_block_hash: [0x77; 32],
            end_height: 512,
        }]
    );

    // compact_block / stream_compact are deliberately skipped here: the mock
    // hardcodes them to None/empty (see dto::compact_block's own tests, which
    // exercise that conversion directly against a hand-built domain value
    // instead).

    println!("\nEvery read round-tripped through its DTO correctly. Done.");
}

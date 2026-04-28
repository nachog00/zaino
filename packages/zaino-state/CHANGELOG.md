## 0.1.0

### Features

- `rpc::grpc::service.rs`, `backends::fetch::get_taddress_transactions`:
    implement the GetTaddressTransactions GRPC method of
    lightclient-protocol v0.4.0 which replaces `GetTaddressTxids`
- `chain_index`
  - `::finalised_state::db::v0::get_compact_block_stream`
  - `::finalised_state::db::v1::get_compact_block_stream`
  - `::types::db::legacy`: `compact_vin`, `compact_vout`,
    `to_compact` (returns compactTx from TxInCompact)
  - new type: `non_finalized_state::ChainIndexSnapshot`
  - `NonFinalizedSnapshot` trait: new method `max_serviceable_height`
  - `::types`
    - new submodule `primitives` with type `BlockIndex { height, hash }`
    - new submodule `block_context` with type `BlockContext { index, parent_hash, chainwork }`
    - new submodule `wire` with business-to-gRPC conversions:
      `BlockIndex::to_wire()`, `BlockIndex::try_from_wire()`,
      error enum `WireBlockIdError`
- `local_cache::compact_block_with_pool_types`

### Changes

- `get_mempool_tx` now takes `GetMempoolTxRequest` as parameter
- `chain_index::finalised_state::db` `get_compact_block` functions
  now take a `PoolTypeFilter` parameter
- `chain_index::types::db::legacy::to_compact_block()` now returns
  transparent data
- `ChainIndex::snapshot_nonfinalized_state` now returns a
  `Future<Output = Result<Self::Snapshot>>`
- `non_finalized_state::BestTip` renamed to `chain_index::types::BlockIndex`

### Deprecated

- `GetTaddressTxids` replaced by `GetTaddressTransactions`

### Removed

- `Ping` GRPC service
- `utils::blockid_to_hashorheight` moved to `zaino_proto::utils`
- `non_finalized_state::NonfinalizedBlockCacheSnapshot` narrowed to `pub(crate)`

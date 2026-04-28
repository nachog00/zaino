## 0.1.1-rc.0 (2026-04-28)

### Fixes

- Placeholder changeset for release flow testing.

## 0.1.0

### Features

- `ValidatedBlockRangeRequest` type that encapsulates validations of the
  `GetBlockRange` RPC request
- utils submodule to handle `PoolType` conversions
- `PoolTypeError` defines conversion errors between i32 and known `PoolType` variants
- `PoolTypeFilter` indicates which pools need to be returned in a compact block.

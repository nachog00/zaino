## 0.1.1-rc.1 (2026-04-28)

### Fixes

- Placeholder changeset for release flow testing.

## 0.1.1-rc.0 (2026-04-28)

### Fixes

- Placeholder changeset for release flow testing.

## 0.1.0

### Changes

- [943] Zallet regtest fixes
- `JsonRpSeeConnector::get_tree_state` now returns a `GetTreestateResponse`
  whose `sapling` and `orchard` fields are optional. In regtest mode, these
  fields may be omitted when the corresponding network upgrade activation
  height is not configured.

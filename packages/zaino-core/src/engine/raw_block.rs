//! Raw block bytes — always passthrough.
//!
//! The raw consensus bytes of a block can only come from the validator: the
//! finalised store holds compact projections, not full block bytes. Like the
//! full [`block`](super::super::engine::block) read, placement never varies —
//! one impl, bounded on the raw-block source ports. A [`BlockSelector`] routes
//! to the by-height or by-hash port.

use crate::chain_view::ChainTier;
use crate::routing::Routing;
use zaino_primitives::types::BlockSelector;
use zaino_service::RawBlockRead;
use zaino_service::error::BlockReadError;
use zaino_source::{GetRawBlock, GetRawBlockByHash};

use super::EngineSnapshot;

impl<F, N, Src, R> RawBlockRead for EngineSnapshot<F, N, Src, R>
where
    F: ChainTier,
    N: ChainTier,
    Src: GetRawBlock + GetRawBlockByHash + Send + Sync + 'static,
    R: Routing,
{
    async fn raw_block(&self, at: BlockSelector) -> Result<Option<Vec<u8>>, BlockReadError> {
        match at {
            BlockSelector::Height(height) => self.passthrough().raw_block(height).await,
            BlockSelector::Hash(hash) => self.passthrough().raw_block_by_hash(hash).await,
        }
    }
}

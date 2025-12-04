use std::str::FromStr;
use std::sync::Arc;
use hex::FromHex;
use accumulators::store::StoreError;
use async_trait::async_trait;

use raito_spv_verify::ChainState;
use zebra_chain::block::Header;
use zebra_chain::block::Hash;

const BLOCKS_PER_EPOCH: u32 = 2016;

#[async_trait]
pub trait ChainStateStore: Send + Sync {
    async fn add_block_header(
        &self,
        height: u32,
        block_header: &Header,
    ) -> Result<(), StoreError>;
    async fn get_block_headers(
        &self,
        start_height: u32,
        num_blocks: u32,
    ) -> Result<Vec<Header>, StoreError>;
    async fn get_block_height(&self, block_hash: &Hash) -> Result<u32, StoreError>;
    async fn add_chain_state(
        &self,
        height: u32,
        chain_state: &ChainState,
    ) -> Result<(), StoreError>;
    async fn get_chain_state(&self, height: u32) -> Result<ChainState, StoreError>;
}

pub struct ChainStateManager {
    current_state: ChainState,
    store: Arc<dyn ChainStateStore>,
}

impl ChainStateManager {
    pub async fn restore(
        store: Arc<dyn ChainStateStore>,
        height: u32,
    ) -> Result<Self, anyhow::Error> {
        let current_state = if height == 0 {
            Self::genesis_state()
        } else {
            store.get_chain_state(height - 1).await?
        };
        Ok(Self {
            current_state,
            store,
        })
    }

    pub async fn update(
        &mut self,
        block_height: u32,
        block_header: &Header,
    ) -> Result<(), anyhow::Error> {
        let new_state = if block_height == 0 {
            self.current_state.clone()
        } else {
            let mut prev_timestamps = self.current_state.prev_timestamps.clone();
            prev_timestamps.push(block_header.time.timestamp() as u32);
            if prev_timestamps.len() > 11 {
                prev_timestamps.remove(0);
            }

            let epoch_start_time = if block_height % BLOCKS_PER_EPOCH == 0 {
                block_header.time.timestamp() as u32
            } else {
                self.current_state.epoch_start_time
            };
                    
            ChainState {
                block_height,
                total_work: self.current_state.total_work, // + block_header.difficulty_threshold, // Question Paul: how do we compute this?
                best_block_hash: block_header.hash(),
                n_bits,
                epoch_start_time,
                prev_timestamps,
            }
        };

        self.store.add_chain_state(block_height, &new_state).await?;
        self.store
            .add_block_header(block_height, block_header)
            .await?;
        self.current_state = new_state;

        Ok(())
    }

    pub fn genesis_state() -> ChainState {
        ChainState {
            block_height: 0,
            total_work: 0x2000,
            best_block_hash: Hash::from_hex("00040fe8ec8471911baa1db1266ea15dd06b4a8a5c453883c000b031973dce08").unwrap(),
            current_target: Target::from_hex("0007ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff").unwrap(),
            prev_timestamps: vec![1477641360],
            epoch_start_time: 1477641360,
            pow_target_history:  [Target::from_hex("0007ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff").unwrap(); 17].to_vec(),
        }
    }
}

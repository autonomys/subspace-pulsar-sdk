use std::marker::PhantomData;
use std::sync::Arc;

use sc_client_api::Backend;
use sc_executor::RuntimeVersionOf;
use sc_service::{BuildGenesisBlock, GenesisBlockBuilder};
use sp_core::H256;
use sp_domains::{DomainId, DomainInstanceData, RuntimeType};
use sp_runtime::traits::{Block as BlockT, Header};

/// [`DomainGenesisBlockBuilder`] is used on the consensus node for building the
/// domain genesis block from a specific serialized domain runtime genesis
/// config.
pub struct DomainGenesisBlockBuilder<Block, B, E> {
    backend: Arc<B>,
    executor: E,
    _phantom: PhantomData<Block>,
}

impl<Block, B, E> DomainGenesisBlockBuilder<Block, B, E>
where
    Block: BlockT,
    B: Backend<Block>,
    E: RuntimeVersionOf + Clone,
{
    /// Constructs a new instance of [`DomainGenesisBlockBuilder`].
    pub fn new(backend: Arc<B>, executor: E) -> Self {
        Self { backend, executor, _phantom: Default::default() }
    }

    /// Constructs the genesis domain block from a serialized runtime genesis
    /// config.
    pub fn generate_genesis_block(
        &self,
        domain_id: DomainId,
        domain_instance_data: DomainInstanceData,
    ) -> sp_blockchain::Result<Block> {
        let DomainInstanceData { runtime_type, runtime_code, raw_genesis_config } =
            domain_instance_data;
        let domain_genesis_block_builder = match runtime_type {
            RuntimeType::Evm => {
                let mut runtime_cfg = match raw_genesis_config {
                    Some(raw_genesis_config) => serde_json::from_slice(&raw_genesis_config)
                        .map_err(|_| {
                            sp_blockchain::Error::Application(Box::from(
                                "Failed to deserialize genesis config of the evm domain",
                            ))
                        })?,
                    None => evm_domain_runtime::RuntimeGenesisConfig::default(),
                };
                runtime_cfg.system.code = runtime_code;
                runtime_cfg.self_domain_id.domain_id = Some(domain_id);
                GenesisBlockBuilder::new(
                    &runtime_cfg,
                    false,
                    self.backend.clone(),
                    self.executor.clone(),
                )?
            }
        };
        domain_genesis_block_builder.build_genesis_block().map(|(genesis_block, _)| genesis_block)
    }
}

impl<Block, B, E> sp_domains::GenerateGenesisStateRoot for DomainGenesisBlockBuilder<Block, B, E>
where
    Block: BlockT,
    Block::Hash: Into<H256>,
    B: Backend<Block>,
    E: RuntimeVersionOf + Clone + Send + Sync,
{
    fn generate_genesis_state_root(
        &self,
        domain_id: DomainId,
        domain_instance_data: DomainInstanceData,
    ) -> Option<H256> {
        self.generate_genesis_block(domain_id, domain_instance_data)
            .map(|genesis_block| *genesis_block.header().state_root())
            .ok()
            .map(Into::into)
    }
}

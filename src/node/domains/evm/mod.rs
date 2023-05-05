//! Core evm domain module

use std::path::Path;

use anyhow::Context;
use core_evm_runtime::{AccountId as AccountId20, Runtime, RuntimeApi};
use cross_domain_message_gossip::GossipWorkerBuilder;
use derivative::Derivative;
use derive_builder::Builder;
use futures::prelude::*;
use serde::{Deserialize, Serialize};
use sp_core::crypto::AccountId32;
use sp_core::ByteArray;
use sp_domains::DomainId;

use super::core::CoreDomainNode;
use crate::node::{Base, BaseBuilder, BlockHeader};

pub(crate) mod chain_spec;

pub(crate) struct AccountId32ToAccountId20Converter;

impl sp_runtime::traits::Convert<AccountId32, AccountId20> for AccountId32ToAccountId20Converter {
    fn convert(acc: AccountId32) -> AccountId20 {
        // Using the full hex key, truncating to the first 20 bytes (the first 40 hex
        // chars)
        sp_core::H160::from_slice(&acc.as_slice()[0..20]).into()
    }
}

pub(crate) struct ExecutorDispatch;

impl sc_executor::NativeExecutionDispatch for ExecutorDispatch {
    // #[cfg(feature = "runtime-benchmarks")]
    // type ExtendHostFunctions = frame_benchmarking::benchmarking::HostFunctions;
    // #[cfg(not(feature = "runtime-benchmarks"))]
    type ExtendHostFunctions = ();

    fn dispatch(method: &str, data: &[u8]) -> Option<Vec<u8>> {
        core_evm_runtime::api::dispatch(method, data)
    }

    fn native_version() -> sc_executor::NativeVersion {
        core_evm_runtime::native_version()
    }
}

/// Node builder
#[derive(Clone, Derivative, Builder, Deserialize, Serialize)]
#[derivative(Debug, PartialEq)]
#[builder(pattern = "immutable", build_fn(private, name = "_build"), name = "ConfigBuilder")]
#[non_exhaustive]
pub struct Config {
    /// Maximum number of logs in a query.
    #[builder(setter(strip_option), default = "10000")]
    pub max_past_logs: u32,
    /// Maximum fee history cache size.
    #[builder(setter(strip_option), default = "2048")]
    pub fee_history_limit: u64,
    /// Enable dev signer
    #[builder(setter(strip_option), default)]
    pub enable_dev_signer: bool,
    /// The dynamic-fee pallet target gas price set by block author
    #[builder(setter(strip_option), default = "1")]
    pub target_gas_price: u64,
    /// Maximum allowed gas limit will be `block.gas_limit *
    /// execute_gas_limit_multiplier` when using eth_call/eth_estimateGas.
    #[builder(setter(strip_option), default = "10")]
    pub execute_gas_limit_multiplier: u64,
    /// Size in bytes of the LRU cache for block data.
    #[builder(setter(strip_option), default = "50")]
    pub eth_log_block_cache: usize,
    /// Size in bytes of the LRU cache for transactions statuses data.
    #[builder(setter(strip_option), default = "50")]
    pub eth_statuses_cache: usize,

    /// Id of the relayer
    #[builder(setter(strip_option), default)]
    #[serde(default, skip_serializing_if = "sdk_utils::is_default")]
    pub relayer_id: Option<core_evm_runtime::AccountId>,
    #[doc(hidden)]
    #[builder(
        setter(into, strip_option),
        field(type = "BaseBuilder", build = "self.base.build()")
    )]
    #[serde(flatten, skip_serializing_if = "sdk_utils::is_default")]
    pub base: Base,
}

sdk_utils::generate_builder!(Config);
crate::derive_base!(crate::node::Base => ConfigBuilder);

/// Chain spec of the core domain
pub type ChainSpec = chain_spec::ChainSpec;

/// Core domain node
#[derive(Clone, Derivative)]
#[derivative(Debug)]
pub struct EvmDomainNode {
    core: CoreDomainNode<RuntimeApi, ExecutorDispatch>,
}

impl EvmDomainNode {
    pub(crate) async fn new(
        cfg: Config,
        directory: impl AsRef<Path>,
        chain_spec: ChainSpec,
        primary_chain_node: &mut crate::node::NewFull,
        system_domain_node: &super::NewFull,
        gossip_worker_builder: &mut GossipWorkerBuilder,
    ) -> anyhow::Result<Self> {
        let Config {
            max_past_logs,
            fee_history_limit,
            enable_dev_signer,
            target_gas_price,
            execute_gas_limit_multiplier,
            eth_log_block_cache,
            eth_statuses_cache,
            relayer_id,
            base,
        } = cfg;
        let provider = domain_eth_service::provider::EthProvider::<
            core_evm_runtime::TransactionConverter,
            domain_eth_service::DefaultEthConfig<
                super::core::FullClient<RuntimeApi, ExecutorDispatch>,
                domain_service::FullBackend<domain_runtime_primitives::opaque::Block>,
            >,
        >::with_configuration(
            Some(directory.as_ref().join(chain_spec.id()).into()),
            domain_eth_service::EthConfiguration {
                max_past_logs,
                fee_history_limit,
                enable_dev_signer,
                target_gas_price,
                execute_gas_limit_multiplier,
                eth_log_block_cache,
                eth_statuses_cache,
            },
        );
        let cfg = super::core::Config {
            base,
            relayer_id,
            directory: directory.as_ref().to_owned(),
            primary_chain_node,
            system_domain_node,
            gossip_worker_builder,
            domain_id: DomainId::CORE_EVM,
            chain_spec,
            provider,
        };
        let core =
            CoreDomainNode::new(cfg).await.context("Failed to build core payments domain")?;

        Ok(Self { core })
    }

    /// Subscribe to new blocks imported
    pub async fn subscribe_new_heads(
        &self,
    ) -> anyhow::Result<impl Stream<Item = BlockHeader> + Send + Sync + Unpin + 'static> {
        Ok(self
            .core
            .rpc()
            .subscribe_new_heads::<Runtime>()
            .await
            .context("Failed to subscribe to new blocks")?
            .map(Into::into))
    }

    /// Subscribe to finalized blocks
    pub async fn subscribe_finalized_heads(
        &self,
    ) -> anyhow::Result<impl Stream<Item = BlockHeader> + Send + Sync + Unpin + 'static> {
        Ok(self
            .core
            .rpc()
            .subscribe_finalized_heads::<Runtime>()
            .await
            .context("Failed to subscribe to finalized blocks")?
            .map(Into::into))
    }
}

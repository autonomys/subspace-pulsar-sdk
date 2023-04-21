//! Core ethereum relay domain module

use std::path::Path;

use anyhow::Context;
use derivative::Derivative;
use derive_builder::Builder;
use futures::prelude::*;
use serde::{Deserialize, Serialize};
use sp_domains::DomainId;

use super::core::CoreDomainNode;
use crate::node::{Base, BaseBuilder, BlockNotification};

pub(crate) mod chain_spec;

pub(crate) struct ExecutorDispatch;

impl sc_executor::NativeExecutionDispatch for ExecutorDispatch {
    // #[cfg(feature = "runtime-benchmarks")]
    // type ExtendHostFunctions = frame_benchmarking::benchmarking::HostFunctions;
    // #[cfg(not(feature = "runtime-benchmarks"))]
    type ExtendHostFunctions = ();

    fn dispatch(method: &str, data: &[u8]) -> Option<Vec<u8>> {
        core_eth_relay_runtime::api::dispatch(method, data)
    }

    fn native_version() -> sc_executor::NativeVersion {
        core_eth_relay_runtime::native_version()
    }
}

/// Node builder
#[derive(Clone, Derivative, Builder, Deserialize, Serialize)]
#[derivative(Debug, PartialEq)]
#[builder(pattern = "immutable", build_fn(private, name = "_build"), name = "ConfigBuilder")]
#[non_exhaustive]
pub struct Config {
    /// Id of the relayer
    #[builder(setter(strip_option), default)]
    #[serde(default, skip_serializing_if = "crate::utils::is_default")]
    pub relayer_id: Option<domain_runtime_primitives::AccountId>,
    #[doc(hidden)]
    #[builder(
        setter(into, strip_option),
        field(type = "BaseBuilder", build = "self.base.build()")
    )]
    #[serde(flatten, skip_serializing_if = "crate::utils::is_default")]
    pub base: Base,
}

crate::derive_base!(crate::node::Base => ConfigBuilder);

impl ConfigBuilder {
    /// Constructor
    pub fn new() -> Self {
        Self::default()
    }

    /// Build Config
    pub fn build(&self) -> Config {
        self._build().expect("Infallible")
    }
}

/// Chain spec of the core domain
pub type ChainSpec = chain_spec::ChainSpec;

/// Core domain node
#[derive(Clone, Derivative)]
#[derivative(Debug)]
pub struct EthDomainNode {
    core: CoreDomainNode<
        domain_runtime_primitives::AccountId,
        core_eth_relay_runtime::RuntimeApi,
        ExecutorDispatch,
    >,
}

impl EthDomainNode {
    pub(crate) async fn new(
        cfg: Config,
        directory: impl AsRef<Path>,
        chain_spec: ChainSpec,
        primary_chain_node: &mut crate::node::NewFull,
        system_domain_node: &super::NewFull,
        gossip_message_sink: domain_client_message_relayer::GossipMessageSink,
        domain_tx_pool_sinks: &mut impl Extend<(
            DomainId,
            cross_domain_message_gossip::DomainTxPoolSink,
        )>,
    ) -> anyhow::Result<Self> {
        let Config { base, relayer_id } = cfg;
        let cfg = super::core::Config {
            base,
            relayer_id,
            directory: directory.as_ref().to_owned(),
            primary_chain_node,
            system_domain_node,
            gossip_message_sink,
            domain_tx_pool_sinks,
            domain_id: DomainId::CORE_ETH_RELAY,
            chain_spec,
        };
        let core = CoreDomainNode::new(cfg).await.context("Failed to build core payments domain")?;

        Ok(Self { core })
    }

    /// Subscribe to new blocks imported
    pub async fn subscribe_new_blocks(
        &self,
    ) -> anyhow::Result<impl Stream<Item = BlockNotification> + Send + Sync + Unpin + 'static> {
        Ok(self
            .core
            .rpc()
            .subscribe_new_blocks::<core_eth_relay_runtime::Runtime>()
            .await
            .context("Failed to subscribe to new blocks")?
            .map(Into::into))
    }
}

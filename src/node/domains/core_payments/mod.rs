//! Core payments domain module

use std::path::Path;

use anyhow::Context;
use core_payments_domain_runtime::{Runtime, RuntimeApi};
use cross_domain_message_gossip::GossipWorkerBuilder;
use derivative::Derivative;
use derive_builder::Builder;
use domain_runtime_primitives::AccountId;
use futures::prelude::*;
use serde::{Deserialize, Serialize};
use sp_domains::DomainId;

use super::core::CoreDomainNode;
use super::BlockHeader;
use crate::node::{Base, BaseBuilder};

pub(crate) mod chain_spec;

/// Core payments domain instance.
pub(crate) struct ExecutorDispatch;

impl sc_executor::NativeExecutionDispatch for ExecutorDispatch {
    // #[cfg(feature = "runtime-benchmarks")]
    // type ExtendHostFunctions = frame_benchmarking::benchmarking::HostFunctions;
    // #[cfg(not(feature = "runtime-benchmarks"))]
    type ExtendHostFunctions = ();

    fn dispatch(method: &str, data: &[u8]) -> Option<Vec<u8>> {
        core_payments_domain_runtime::api::dispatch(method, data)
    }

    fn native_version() -> sc_executor::NativeVersion {
        core_payments_domain_runtime::native_version()
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
    #[serde(default, skip_serializing_if = "sdk_utils::is_default")]
    pub relayer_id: Option<AccountId>,
    #[doc(hidden)]
    #[builder(
        setter(into, strip_option),
        field(type = "BaseBuilder", build = "self.base.build()")
    )]
    #[serde(flatten, skip_serializing_if = "sdk_utils::is_default")]
    pub base: Base,
}

crate::generate_builder!(Config);
crate::derive_base!(crate::node::Base => ConfigBuilder);

/// Chain spec of the core domain
pub type ChainSpec = chain_spec::ChainSpec;

/// Core domain node
#[derive(Clone, Derivative)]
#[derivative(Debug)]
pub struct CorePaymentsDomainNode {
    core: CoreDomainNode<RuntimeApi, ExecutorDispatch>,
}

impl CorePaymentsDomainNode {
    pub(crate) async fn new(
        cfg: Config,
        directory: impl AsRef<Path>,
        chain_spec: ChainSpec,
        primary_chain_node: &mut crate::node::NewFull,
        system_domain_node: &super::NewFull,
        gossip_worker_builder: &mut GossipWorkerBuilder,
    ) -> anyhow::Result<Self> {
        let Config { base, relayer_id } = cfg;
        let cfg = super::core::Config {
            base,
            relayer_id,
            directory: directory.as_ref().to_owned(),
            primary_chain_node,
            system_domain_node,
            gossip_worker_builder,
            domain_id: DomainId::CORE_PAYMENTS,
            chain_spec,
            provider: domain_service::providers::DefaultProvider,
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

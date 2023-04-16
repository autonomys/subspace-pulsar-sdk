//! Core payments domain module

use std::path::Path;
use std::sync::{Arc, Weak};

use anyhow::Context;
use core_payments_domain_runtime::RelayerId;
use derivative::Derivative;
use derive_builder::Builder;
use domain_service::DomainConfiguration;
use futures::prelude::*;
use sc_client_api::BlockchainEvents;
use serde::{Deserialize, Serialize};
use sp_domains::DomainId;

use super::BlockNotification;
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
    #[serde(default, skip_serializing_if = "crate::utils::is_default")]
    pub relayer_id: Option<RelayerId>,
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

pub(crate) type FullClient =
    domain_service::FullClient<core_payments_domain_runtime::RuntimeApi, ExecutorDispatch>;
pub(crate) type NewFull = domain_service::NewFullCore<
    Arc<FullClient>,
    sc_executor::NativeElseWasmExecutor<ExecutorDispatch>,
    sp_runtime::generic::Block<
        sp_runtime::generic::Header<u32, sp_runtime::traits::BlakeTwo256>,
        sp_runtime::OpaqueExtrinsic,
    >,
    subspace_runtime_primitives::opaque::Block,
    super::FullClient,
    crate::node::FullClient,
    core_payments_domain_runtime::RuntimeApi,
    ExecutorDispatch,
>;

/// Chain spec of the core domain
pub type ChainSpec = chain_spec::ChainSpec;

/// Core domain node
#[derive(Clone, Derivative)]
#[derivative(Debug)]
pub struct CoreDomainNode {
    #[derivative(Debug = "ignore")]
    _client: Weak<FullClient>,
    rpc_handlers: crate::utils::Rpc,
}

impl CoreDomainNode {
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
        let Config { base, relayer_id: maybe_relayer_id } = cfg;
        let service_config = base.configuration(directory, chain_spec).await;
        let core_domain_config = DomainConfiguration { service_config, maybe_relayer_id };

        // TODO: proper value
        let block_import_throttling_buffer_size = 10;
        let block_importing_notification_stream = primary_chain_node
            .block_importing_notification_stream
            .subscribe()
            .then(|block_importing_notification| async move {
                (
                    block_importing_notification.block_number,
                    block_importing_notification.acknowledgement_sender,
                )
            });
        let new_slot_notification_stream = primary_chain_node
            .new_slot_notification_stream
            .subscribe()
            .then(|slot_notification| async move {
                (
                    slot_notification.new_slot_info.slot,
                    slot_notification.new_slot_info.global_challenge,
                    None,
                )
            });

        let executor_streams = domain_client_executor::ExecutorStreams {
            primary_block_import_throttling_buffer_size: block_import_throttling_buffer_size,
            block_importing_notification_stream,
            imported_block_notification_stream: primary_chain_node
                .client
                .every_import_notification_stream(),
            new_slot_notification_stream,
            _phantom: Default::default(),
        };

        let core_domain_params = domain_service::CoreDomainParams {
            domain_id: DomainId::CORE_PAYMENTS,
            core_domain_config,
            system_domain_client: system_domain_node.client.clone(),
            system_domain_sync_service: system_domain_node.sync_service.clone(),
            primary_chain_client: primary_chain_node.client.clone(),
            primary_network_sync_oracle: primary_chain_node.sync_service.clone(),
            select_chain: primary_chain_node.select_chain.clone(),
            executor_streams,
            gossip_message_sink,
        };

        let NewFull { client, rpc_handlers, tx_pool_sink, task_manager, network_starter, .. } =
            domain_service::new_full_core(core_domain_params).await?;

        domain_tx_pool_sinks.extend([(DomainId::CORE_PAYMENTS, tx_pool_sink)]);
        primary_chain_node.task_manager.add_child(task_manager);

        network_starter.start_network();

        Ok(Self {
            _client: Arc::downgrade(&client),
            rpc_handlers: crate::utils::Rpc::new(&rpc_handlers),
        })
    }

    pub(crate) fn _client(&self) -> anyhow::Result<Arc<FullClient>> {
        self._client.upgrade().ok_or_else(|| anyhow::anyhow!("The node was already closed"))
    }

    /// Subscribe to new blocks imported
    pub async fn subscribe_new_blocks(
        &self,
    ) -> anyhow::Result<impl Stream<Item = BlockNotification> + Send + Sync + Unpin + 'static> {
        Ok(self
            .rpc_handlers
            .subscribe_new_blocks::<core_payments_domain_runtime::Runtime>()
            .await
            .context("Failed to subscribe to new blocks")?
            .map(Into::into))
    }
}

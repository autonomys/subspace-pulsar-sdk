//! Core payments domain module

use std::path::Path;
use std::sync::{Arc, Weak};

use anyhow::Context;
use core_payments_domain_runtime::RelayerId;
use derivative::Derivative;
use derive_builder::Builder;
use domain_service::DomainConfiguration;
use futures::prelude::*;
use serde::{Deserialize, Serialize};
use sp_domains::DomainId;

use crate::node::{Base, BaseBuilder, BlockNotification};

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
    #[derivative(Debug = "ignore", PartialEq = "ignore")]
    #[builder(setter(skip), field(type = "()", build = "None"))]
    #[serde(skip)]
    chain_spec: Option<ChainSpec>,
}

crate::derive_base!(crate::node::Base => ConfigBuilder);

impl ConfigBuilder {
    /// Constructor
    pub fn new() -> Self {
        Self::default()
    }

    /// Build Config
    pub fn build(&self, chain_spec: ChainSpec) -> Config {
        Config { chain_spec: Some(chain_spec), ..self._build().expect("Infallible") }
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
pub type ChainSpec =
    sc_subspace_chain_specs::ExecutionChainSpec<core_payments_domain_runtime::GenesisConfig>;

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
        primary_chain_node: &mut crate::node::NewFull,
        system_domain_node: &super::NewFull,
        gossip_msg_sink: domain_client_message_relayer::GossipMessageSink,
        domain_tx_pool_sinks: &mut impl Extend<(
            DomainId,
            cross_domain_message_gossip::DomainTxPoolSink,
        )>,
    ) -> anyhow::Result<Self> {
        let Config { base, relayer_id: maybe_relayer_id, chain_spec } = cfg;
        let chain_spec = chain_spec.expect("Always set in builder");
        let service_config = base.configuration(directory, chain_spec).await;
        let core_domain_config = DomainConfiguration { service_config, maybe_relayer_id };

        // TODO: proper value
        let block_import_throttling_buffer_size = 10;
        let imported_block_notification_stream = primary_chain_node
            .imported_block_notification_stream
            .subscribe()
            .then(|imported_block_notification| async move {
                (
                    imported_block_notification.block_number,
                    imported_block_notification.fork_choice,
                    imported_block_notification.block_import_acknowledgement_sender,
                )
            });
        let new_slot_notification_stream = primary_chain_node
            .new_slot_notification_stream
            .subscribe()
            .then(|slot_notification| async move {
                (
                    slot_notification.new_slot_info.slot,
                    slot_notification.new_slot_info.global_challenge,
                )
            });

        let NewFull { client, rpc_handlers, tx_pool_sink, task_manager, network_starter, .. } =
            domain_service::new_full_core(
                DomainId::CORE_PAYMENTS,
                core_domain_config,
                system_domain_node.client.clone(),
                system_domain_node.network.clone(),
                primary_chain_node.client.clone(),
                primary_chain_node.network.clone(),
                &primary_chain_node.select_chain,
                imported_block_notification_stream,
                new_slot_notification_stream,
                block_import_throttling_buffer_size,
                gossip_msg_sink,
            )
            .await?;

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
        self.rpc_handlers.subscribe_new_blocks().await.context("Failed to subscribe to new blocks")
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::farmer::CacheDescription;
    use crate::node::{chain_spec, domains, Role};
    use crate::{Farmer, Node, PlotDescription};

    #[tokio::test(flavor = "multi_thread")]
    async fn test_core() {
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();

        let dir = TempDir::new().unwrap();
        let core = ConfigBuilder::new().build(chain_spec::core_payments::development_config());
        let node = Node::builder()
            .system_domain(domains::ConfigBuilder::new().core(core))
            .force_authoring(true)
            .role(Role::Authority)
            .build(dir.path(), chain_spec::dev_config().unwrap())
            .await
            .unwrap();
        let (plot_dir, cache_dir) = (TempDir::new().unwrap(), TempDir::new().unwrap());
        let farmer = Farmer::builder()
            .build(
                Default::default(),
                node.clone(),
                &[PlotDescription::new(plot_dir.as_ref(), bytesize::ByteSize::gb(1)).unwrap()],
                CacheDescription::new(cache_dir.as_ref(), CacheDescription::MIN_SIZE).unwrap(),
            )
            .await
            .unwrap();

        node.system_domain()
            .unwrap()
            .core()
            .unwrap()
            .subscribe_new_blocks()
            .await
            .unwrap()
            .next()
            .await
            .unwrap();

        farmer.close().await.unwrap();
        node.close().await;
    }
}

use std::path::Path;
use std::sync::{Arc, Weak};

use anyhow::Context;
use core_payments_domain_runtime::RelayerId;
use derivative::Derivative;
use derive_builder::Builder;
use domain_service::DomainConfiguration;
use futures::prelude::*;
use sc_client_api::client::BlockImportNotification;
use sc_subspace_chain_specs::ConsensusChainSpec;
use serde::{Deserialize, Serialize};
use sp_domains::DomainId;
use subspace_runtime::{Block, GenesisConfig as ConsensusGenesisConfig};
use subspace_runtime_primitives::opaque::Header;
use system_domain_runtime::GenesisConfig as ExecutionGenesisConfig;

use crate::node::{Base, BaseBuilder, BlockNotification};

/// System domain executor instance.
pub(crate) struct ExecutorDispatch;

impl sc_executor::NativeExecutionDispatch for ExecutorDispatch {
    // #[cfg(feature = "runtime-benchmarks")]
    // type ExtendHostFunctions = frame_benchmarking::benchmarking::HostFunctions;
    // #[cfg(not(feature = "runtime-benchmarks"))]
    type ExtendHostFunctions = ();

    fn dispatch(method: &str, data: &[u8]) -> Option<Vec<u8>> {
        system_domain_runtime::api::dispatch(method, data)
    }

    fn native_version() -> sc_executor::NativeVersion {
        system_domain_runtime::native_version()
    }
}

/// Node builder
#[derive(Debug, Clone, Derivative, Builder, Deserialize, Serialize, PartialEq)]
#[derivative(Default)]
#[builder(pattern = "immutable", build_fn(private, name = "_build"), name = "ConfigBuilder")]
#[non_exhaustive]
pub struct Config {
    #[doc(hidden)]
    #[builder(
        setter(into, strip_option),
        field(type = "BaseBuilder", build = "self.base.build()")
    )]
    #[serde(default)]
    pub base: Base,
    /// Id of the relayer
    #[builder(setter(strip_option), default)]
    #[serde(default)]
    pub relayer_id: Option<RelayerId>,
}

pub(crate) type FullClient =
    domain_service::FullClient<system_domain_runtime::RuntimeApi, ExecutorDispatch>;
pub(crate) type NewFull = domain_service::NewFull<
    Arc<FullClient>,
    sc_executor::NativeElseWasmExecutor<ExecutorDispatch>,
    subspace_runtime_primitives::opaque::Block,
    crate::node::PrimaryFullClient,
    system_domain_runtime::RuntimeApi,
    ExecutorDispatch,
>;
pub(crate) type NetworkService = sc_network::NetworkService<
    sp_runtime::generic::Block<
        sp_runtime::generic::Header<u32, sp_runtime::traits::BlakeTwo256>,
        sp_runtime::OpaqueExtrinsic,
    >,
    super::Hash,
>;

/// Secondary executor node
#[derive(Clone)]
pub struct SecondaryNode {
    client: Weak<FullClient>,
    network: Weak<NetworkService>,
    gossip_msg_sink: domain_client_message_relayer::GossipMessageSink,
    _rpc_handlers: crate::utils::Rpc,
}

impl SecondaryNode {
    pub(crate) async fn new(
        cfg: Config,
        directory: impl AsRef<Path>,
        chain_spec: ConsensusChainSpec<ConsensusGenesisConfig, ExecutionGenesisConfig>,
        primary_new_full: &mut crate::node::PrimaryNewFull,
    ) -> anyhow::Result<Self> {
        let Config { base, relayer_id: maybe_relayer_id } = cfg;
        let service_config = base.configuration(directory, chain_spec).await;
        let crate::node::PrimaryNewFull {
            imported_block_notification_stream,
            new_slot_notification_stream,
            client,
            network,
            select_chain,
            task_manager,
            ..
        } = primary_new_full;

        let secondary_chain_config = DomainConfiguration { service_config, maybe_relayer_id };
        let imported_block_notification_stream = imported_block_notification_stream
            .subscribe()
            .then(|imported_block_notification| async move {
                (
                    imported_block_notification.block_number,
                    imported_block_notification.fork_choice,
                    imported_block_notification.block_import_acknowledgement_sender,
                )
            });

        let new_slot_notification_stream =
            new_slot_notification_stream.subscribe().then(|slot_notification| async move {
                (
                    slot_notification.new_slot_info.slot,
                    slot_notification.new_slot_info.global_challenge,
                )
            });

        let (gossip_msg_sink, gossip_msg_stream) =
            sc_utils::mpsc::tracing_unbounded("Cross domain gossip messages");

        // TODO: proper value
        let block_import_throttling_buffer_size = 10;

        let secondary_chain_node = domain_service::new_full(
            secondary_chain_config,
            client.clone(),
            network.clone(),
            select_chain,
            imported_block_notification_stream,
            new_slot_notification_stream,
            block_import_throttling_buffer_size,
            gossip_msg_sink.clone(),
        )
        .await?;

        let mut domain_tx_pool_sinks = std::collections::BTreeMap::new();
        domain_tx_pool_sinks.insert(DomainId::SYSTEM, secondary_chain_node.tx_pool_sink);

        task_manager.add_child(secondary_chain_node.task_manager);
        let cross_domain_message_gossip_worker =
            cross_domain_message_gossip::GossipWorker::<Block>::new(
                network.clone(),
                domain_tx_pool_sinks,
            );

        let NewFull { client, network_starter, network, rpc_handlers, .. } = secondary_chain_node;

        tokio::spawn(cross_domain_message_gossip_worker.run(gossip_msg_stream));
        network_starter.start_network();

        Ok(Self {
            client: Arc::downgrade(&client),
            network: Arc::downgrade(&network),
            gossip_msg_sink,
            _rpc_handlers: crate::utils::Rpc::new(&rpc_handlers),
        })
    }

    pub(crate) fn client(&self) -> anyhow::Result<Arc<FullClient>> {
        self.client.upgrade().ok_or_else(|| anyhow::anyhow!("The node was already closed"))
    }

    pub(crate) fn network(&self) -> anyhow::Result<Arc<NetworkService>> {
        self.network.upgrade().ok_or_else(|| anyhow::anyhow!("The node was already closed"))
    }

    /// Subscribe to new blocks imported
    pub async fn subscribe_new_blocks(
        &self,
    ) -> anyhow::Result<impl Stream<Item = BlockNotification> + Send + Sync + Unpin + 'static> {
        use sc_client_api::client::BlockchainEvents;

        let stream = self
            .client()
            .context("Failed to subscribe to new blocks")?
            .import_notification_stream()
            .map(
                |BlockImportNotification {
                     hash,
                     header: Header { parent_hash, number, state_root, extrinsics_root, digest: _ },
                     origin: _,
                     is_new_best,
                     tree_route: _,
                 }| BlockNotification {
                    hash,
                    number,
                    parent_hash,
                    state_root,
                    extrinsics_root,
                    is_new_best,
                },
            );
        Ok(stream)
    }
}

crate::generate_builder!(Config);

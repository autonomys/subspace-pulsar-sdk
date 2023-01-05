//! Module for executor and its domains

use std::path::Path;
use std::sync::{Arc, Weak};

use anyhow::Context;
use core_payments_domain_runtime::RelayerId;
use derivative::Derivative;
use derive_builder::Builder;
use domain_service::DomainConfiguration;
use futures::prelude::*;
use sc_client_api::client::BlockImportNotification;
use serde::{Deserialize, Serialize};
use sp_domains::DomainId;
use subspace_runtime::Block;
use subspace_runtime_primitives::opaque::Header;
use system_domain_runtime::GenesisConfig as ExecutionGenesisConfig;

use self::core::CoreNode;
use crate::node::{Base, BaseBuilder, BlockNotification};

pub mod core;

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
    /// Id of the relayer
    #[builder(setter(strip_option), default)]
    #[serde(default, skip_serializing_if = "crate::utils::is_default")]
    pub relayer_id: Option<RelayerId>,
    #[doc(hidden)]
    #[builder(
        setter(into, strip_option),
        field(type = "BaseBuilder", build = "self.base.build()")
    )]
    #[serde(default, skip_serializing_if = "crate::utils::is_default")]
    pub base: Base,
    /// The core config
    #[builder(setter(strip_option), default)]
    #[serde(default, skip_serializing_if = "crate::utils::is_default")]
    pub core: Option<core::Config>,
}

crate::derive_base!(crate::node::Base => ConfigBuilder);

pub(crate) type FullClient =
    domain_service::FullClient<system_domain_runtime::RuntimeApi, ExecutorDispatch>;
pub(crate) type NewFull = domain_service::NewFull<
    Arc<FullClient>,
    sc_executor::NativeElseWasmExecutor<ExecutorDispatch>,
    subspace_runtime_primitives::opaque::Block,
    crate::node::FullClient,
    system_domain_runtime::RuntimeApi,
    ExecutorDispatch,
>;
/// Chain spec of the secondary chain
pub type ChainSpec = sc_subspace_chain_specs::ExecutionChainSpec<ExecutionGenesisConfig>;

/// Secondary executor node
#[derive(Clone, Derivative)]
#[derivative(Debug)]
pub struct SecondaryNode {
    #[derivative(Debug = "ignore")]
    client: Weak<FullClient>,
    core: Option<CoreNode>,
    _rpc_handlers: crate::utils::Rpc,
}

impl SecondaryNode {
    pub(crate) async fn new(
        cfg: Config,
        directory: impl AsRef<Path>,
        chain_spec: ChainSpec,
        primary_new_full: &mut crate::node::NewFull,
    ) -> anyhow::Result<Self> {
        let Config { base, relayer_id: maybe_relayer_id, core } = cfg;
        let service_config =
            base.configuration(directory.as_ref().join("system"), chain_spec).await;

        let secondary_chain_config = DomainConfiguration { service_config, maybe_relayer_id };
        let imported_block_notification_stream = primary_new_full
            .imported_block_notification_stream
            .subscribe()
            .then(|imported_block_notification| async move {
                (
                    imported_block_notification.block_number,
                    imported_block_notification.fork_choice,
                    imported_block_notification.block_import_acknowledgement_sender,
                )
            });

        let new_slot_notification_stream = primary_new_full
            .new_slot_notification_stream
            .subscribe()
            .then(|slot_notification| async move {
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
            primary_new_full.client.clone(),
            primary_new_full.network.clone(),
            &primary_new_full.select_chain,
            imported_block_notification_stream,
            new_slot_notification_stream,
            block_import_throttling_buffer_size,
            gossip_msg_sink.clone(),
        )
        .await?;

        let mut domain_tx_pool_sinks = std::collections::BTreeMap::new();

        let core = if let Some(core) = core {
            CoreNode::new(
                core,
                directory.as_ref().join("core"),
                primary_new_full,
                &secondary_chain_node,
                gossip_msg_sink.clone(),
                &mut domain_tx_pool_sinks,
            )
            .await
            .map(Some)?
        } else {
            None
        };

        domain_tx_pool_sinks.insert(DomainId::SYSTEM, secondary_chain_node.tx_pool_sink);
        primary_new_full.task_manager.add_child(secondary_chain_node.task_manager);

        let cross_domain_message_gossip_worker =
            cross_domain_message_gossip::GossipWorker::<Block>::new(
                primary_new_full.network.clone(),
                domain_tx_pool_sinks,
            );

        let NewFull { client, network_starter, rpc_handlers, .. } = secondary_chain_node;

        tokio::spawn(cross_domain_message_gossip_worker.run(gossip_msg_stream));
        network_starter.start_network();

        Ok(Self {
            client: Arc::downgrade(&client),
            core,
            _rpc_handlers: crate::utils::Rpc::new(&rpc_handlers),
        })
    }

    pub(crate) fn client(&self) -> anyhow::Result<Arc<FullClient>> {
        self.client.upgrade().ok_or_else(|| anyhow::anyhow!("The node was already closed"))
    }

    /// Get the core node handler
    pub fn core(&self) -> Option<CoreNode> {
        self.core.clone()
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

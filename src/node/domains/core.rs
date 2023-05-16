//! Core domain template module

use std::path::PathBuf;
use std::sync::{Arc, Weak};

use derivative::Derivative;
use domain_runtime_primitives::opaque::{Block, Block as SBlock};
use domain_service::DomainConfiguration;
use futures::prelude::*;
use parity_scale_codec::{Decode, Encode};
use sc_client_api::BlockchainEvents;
use sp_api::NumberFor;
use sp_domains::DomainId;
use subspace_runtime_primitives::opaque::Block as PBlock;

use super::FullClient as SClient;
use crate::node::{Base, FullClient as PClient};

type BlockImportOf<Provider, RuntimeApi, ExecutorDispatch> =
    <Provider as domain_service::providers::BlockImportProvider<
        Block,
        FullClient<RuntimeApi, ExecutorDispatch>,
    >>::BI;

pub(crate) type FullClient<RuntimeApi, ExecutorDispatch> =
    domain_service::FullClient<Block, RuntimeApi, ExecutorDispatch>;

pub(crate) type NewFull<RuntimeApi, ExecutorDispatch, AccountId, Provider> =
    domain_service::NewFullCore<
        Arc<FullClient<RuntimeApi, ExecutorDispatch>>,
        sc_executor::NativeElseWasmExecutor<ExecutorDispatch>,
        Block,
        SBlock,
        PBlock,
        SClient,
        PClient,
        RuntimeApi,
        ExecutorDispatch,
        AccountId,
        BlockImportOf<Provider, RuntimeApi, ExecutorDispatch>,
    >;

/// Core domain node
#[derive(Derivative)]
#[derivative(Debug, Clone(bound = ""))]
pub struct CoreDomainNode<RuntimeApi, ExecutorDispatch: sc_executor::NativeExecutionDispatch> {
    #[derivative(Debug = "ignore")]
    client: Weak<FullClient<RuntimeApi, ExecutorDispatch>>,
    rpc_handlers: crate::utils::Rpc,
}

/// Internal config for core domains
pub(crate) struct Config<'a, AccountId, CS, DTXS, Provider> {
    pub base: Base,
    pub relayer_id: Option<AccountId>,
    pub directory: PathBuf,
    pub chain_spec: CS,
    pub(crate) primary_chain_node: &'a mut crate::node::NewFull,
    pub(crate) system_domain_node: &'a super::NewFull,
    pub gossip_message_sink: domain_client_message_relayer::GossipMessageSink,
    pub domain_id: DomainId,
    pub domain_tx_pool_sinks: &'a mut DTXS,
    pub provider: Provider,
}

impl<RuntimeApi, ExecutorDispatch> CoreDomainNode<RuntimeApi, ExecutorDispatch>
where
    ExecutorDispatch: sc_executor::NativeExecutionDispatch + 'static,
    RuntimeApi: sp_api::ConstructRuntimeApi<Block, FullClient<RuntimeApi, ExecutorDispatch>>
        + Send
        + Sync
        + 'static,
    RuntimeApi::RuntimeApi: sp_api::ApiExt<
            Block,
            StateBackend = sc_client_api::backend::StateBackendFor<
                sc_service::TFullBackend<Block>,
                Block,
            >,
        > + sp_api::Metadata<Block>
        + sp_block_builder::BlockBuilder<Block>
        + sp_offchain::OffchainWorkerApi<Block>
        + sp_session::SessionKeys<Block>
        + domain_runtime_primitives::DomainCoreApi<Block>
        + sp_transaction_pool::runtime_api::TaggedTransactionQueue<Block>
        + pallet_transaction_payment_rpc_runtime_api::TransactionPaymentApi<
            Block,
            domain_runtime_primitives::Balance,
        > + sp_messenger::MessengerApi<Block, NumberFor<Block>>
        + domain_runtime_primitives::InherentExtrinsicApi<Block>,
{
    pub(crate) async fn new<CS, DTXS, AccountId, Provider>(
        cfg: Config<'_, AccountId, CS, DTXS, Provider>,
    ) -> anyhow::Result<Self>
    where
        AccountId: serde::de::DeserializeOwned
            + Encode
            + Decode
            + Clone
            + std::fmt::Debug
            + std::fmt::Display
            + std::str::FromStr
            + Sync
            + Send
            + 'static,
        CS: sc_chain_spec::ChainSpec
            + serde::Serialize
            + serde::de::DeserializeOwned
            + sp_runtime::BuildStorage
            + 'static,
        DTXS: Extend<(DomainId, cross_domain_message_gossip::DomainTxPoolSink)>,
        RuntimeApi::RuntimeApi: sp_messenger::RelayerApi<Block, AccountId, NumberFor<Block>>
            + frame_system_rpc_runtime_api::AccountNonceApi<
                Block,
                AccountId,
                subspace_runtime_primitives::Index,
            >,
        Provider: domain_service::providers::RpcProvider<
                Block,
                FullClient<RuntimeApi, ExecutorDispatch>,
                subspace_transaction_pool::FullPool<
                    Block,
                    FullClient<RuntimeApi, ExecutorDispatch>,
                    domain_service::CoreDomainTxPreValidator<
                        Block,
                        SBlock,
                        PBlock,
                        FullClient<RuntimeApi, ExecutorDispatch>,
                        SClient,
                    >,
                >,
                subspace_transaction_pool::FullChainApiWrapper<
                    Block,
                    FullClient<RuntimeApi, ExecutorDispatch>,
                    domain_service::CoreDomainTxPreValidator<
                        Block,
                        SBlock,
                        PBlock,
                        FullClient<RuntimeApi, ExecutorDispatch>,
                        SClient,
                    >,
                >,
                sc_service::TFullBackend<Block>,
                AccountId,
            > + domain_service::providers::BlockImportProvider<
                Block,
                FullClient<RuntimeApi, ExecutorDispatch>,
            > + 'static,
    {
        let Config {
            base,
            relayer_id: maybe_relayer_id,
            directory,
            chain_spec,
            primary_chain_node,
            system_domain_node,
            gossip_message_sink,
            domain_id,
            domain_tx_pool_sinks,
            provider,
        } = cfg;
        let service_config = base.configuration(directory, chain_spec).await;
        let core_domain_config = DomainConfiguration { service_config, maybe_relayer_id };

        // TODO: proper value
        let block_import_throttling_buffer_size = 10;
        let block_importing_notification_stream = primary_chain_node
            .block_importing_notification_stream
            .subscribe()
            .map(|block_importing_notification| {
                (
                    block_importing_notification.block_number,
                    block_importing_notification.acknowledgement_sender,
                )
            });

        let new_slot_notification_stream =
            primary_chain_node.new_slot_notification_stream.subscribe().map(|slot_notification| {
                (
                    slot_notification.new_slot_info.slot,
                    slot_notification.new_slot_info.global_challenge,
                    None,
                )
            });

        let executor_streams = domain_client_executor::ExecutorStreams {
            primary_block_import_throttling_buffer_size: block_import_throttling_buffer_size,
            imported_block_notification_stream: primary_chain_node
                .client
                .every_import_notification_stream(),
            block_importing_notification_stream,
            new_slot_notification_stream,
            _phantom: Default::default(),
        };

        let core_domain_params = domain_service::CoreDomainParams {
            domain_id,
            core_domain_config,
            system_domain_client: system_domain_node.client.clone(),
            system_domain_sync_service: system_domain_node.sync_service.clone(),
            primary_chain_client: primary_chain_node.client.clone(),
            primary_network_sync_oracle: primary_chain_node.sync_service.clone(),
            select_chain: primary_chain_node.select_chain.clone(),
            executor_streams,
            gossip_message_sink,
            provider,
        };

        let NewFull::<_, _, _, Provider> {
            client,
            rpc_handlers,
            tx_pool_sink,
            task_manager,
            network_starter,
            ..
        } = domain_service::new_full_core(core_domain_params).await?;

        domain_tx_pool_sinks.extend([(domain_id, tx_pool_sink)]);
        primary_chain_node.task_manager.add_child(task_manager);

        network_starter.start_network();

        Ok(Self {
            client: Arc::downgrade(&client),
            rpc_handlers: crate::utils::Rpc::new(&rpc_handlers),
        })
    }

    pub(crate) fn _client(&self) -> anyhow::Result<Arc<FullClient<RuntimeApi, ExecutorDispatch>>> {
        self.client.upgrade().ok_or_else(|| anyhow::anyhow!("The node was already closed"))
    }

    pub(crate) fn rpc(&self) -> &crate::utils::Rpc {
        &self.rpc_handlers
    }
}

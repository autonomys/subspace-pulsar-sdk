//! Core domain template module

use std::marker::PhantomData;
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

use crate::node::Base;

pub(crate) type NewFull<Client, RuntimeApi, ExecutorDispatch, AccountId> =
    domain_service::NewFullCore<
        Arc<Client>,
        sc_executor::NativeElseWasmExecutor<ExecutorDispatch>,
        Block,
        SBlock,
        PBlock,
        super::FullClient,
        crate::node::FullClient,
        RuntimeApi,
        ExecutorDispatch,
        AccountId,
    >;
pub(crate) type FullClient<RuntimeApi, ExecutorDispatch> =
    domain_service::FullClient<Block, RuntimeApi, ExecutorDispatch>;

/// Core domain node
#[derive(Derivative)]
#[derivative(Debug, Clone(bound = ""))]
pub struct CoreDomainNode<
    AccountId,
    RuntimeApi,
    ExecutorDispatch: sc_executor::NativeExecutionDispatch,
> {
    #[derivative(Debug = "ignore")]
    client: Weak<FullClient<RuntimeApi, ExecutorDispatch>>,
    rpc_handlers: crate::utils::Rpc,
    #[derivative(Debug = "ignore")]
    _account_id: PhantomData<AccountId>,
}

type BoxedStream<I> = Box<dyn Stream<Item = I> + Send + Sync + Unpin>;

type CoreDomainParams<AccountId> = domain_service::CoreDomainParams<
    SBlock,
    PBlock,
    super::FullClient,
    crate::node::FullClient,
    sc_consensus::LongestChain<sc_client_db::Backend<PBlock>, PBlock>,
    BoxedStream<(NumberFor<PBlock>, futures::channel::mpsc::Sender<()>)>,
    BoxedStream<sc_client_api::BlockImportNotification<PBlock>>,
    BoxedStream<(
        sp_consensus_slots::Slot,
        subspace_core_primitives::Blake2b256Hash,
        Option<futures::channel::mpsc::Sender<()>>,
    )>,
    AccountId,
>;

/// Internal config for core domains
pub(crate) struct Config<'a, AccountId, CS, DTXS> {
    pub base: Base,
    pub relayer_id: Option<AccountId>,
    pub directory: PathBuf,
    pub chain_spec: CS,
    pub(crate) primary_chain_node: &'a mut crate::node::NewFull,
    pub(crate) system_domain_node: &'a super::NewFull,
    pub gossip_message_sink: domain_client_message_relayer::GossipMessageSink,
    pub domain_id: DomainId,
    pub domain_tx_pool_sinks: &'a mut DTXS,
}

impl<AccountId, RuntimeApi, ExecutorDispatch>
    CoreDomainNode<AccountId, RuntimeApi, ExecutorDispatch>
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
        + frame_system_rpc_runtime_api::AccountNonceApi<
            Block,
            AccountId,
            subspace_runtime_primitives::Index,
        > + pallet_transaction_payment_rpc_runtime_api::TransactionPaymentApi<
            Block,
            domain_runtime_primitives::Balance,
        > + sp_messenger::MessengerApi<Block, NumberFor<Block>>
        + sp_messenger::RelayerApi<Block, AccountId, NumberFor<Block>>,
{
    pub(crate) async fn new<CS, DTXS>(cfg: Config<'_, AccountId, CS, DTXS>) -> anyhow::Result<Self>
    where
        CS: sc_chain_spec::ChainSpec
            + serde::Serialize
            + serde::de::DeserializeOwned
            + sp_runtime::BuildStorage
            + 'static,
        DTXS: Extend<(DomainId, cross_domain_message_gossip::DomainTxPoolSink)>,
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
            imported_block_notification_stream: Box::new(
                primary_chain_node.client.every_import_notification_stream(),
            ) as BoxedStream<_>,
            block_importing_notification_stream: Box::new(block_importing_notification_stream)
                as BoxedStream<_>,
            new_slot_notification_stream: Box::new(new_slot_notification_stream) as BoxedStream<_>,
            _phantom: Default::default(),
        };

        let core_domain_params = CoreDomainParams {
            domain_id,
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

        domain_tx_pool_sinks.extend([(domain_id, tx_pool_sink)]);
        primary_chain_node.task_manager.add_child(task_manager);

        network_starter.start_network();

        Ok(Self {
            client: Arc::downgrade(&client),
            rpc_handlers: crate::utils::Rpc::new(&rpc_handlers),
            _account_id: PhantomData,
        })
    }

    pub(crate) fn _client(&self) -> anyhow::Result<Arc<FullClient<RuntimeApi, ExecutorDispatch>>> {
        self.client.upgrade().ok_or_else(|| anyhow::anyhow!("The node was already closed"))
    }

    pub(crate) fn rpc(&self) -> &crate::utils::Rpc {
        &self.rpc_handlers
    }
}

//! Crate with subspace node

#![warn(
    missing_docs,
    clippy::dbg_macro,
    clippy::unwrap_used,
    clippy::disallowed_types,
    unused_features
)]
#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![feature(concat_idents)]

use std::io;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use derivative::Derivative;
use futures::{FutureExt, Stream, StreamExt};
use sc_consensus_subspace::SegmentHeadersStore;
use sc_network::network_state::NetworkState;
use sc_network::{NetworkService, NetworkStateInfo, SyncState};
use sc_rpc_api::state::StateApiClient;
use sdk_dsn::{DsnOptions, DsnShared, NodePieceCache};
use sdk_traits::Farmer;
use sdk_utils::{DestructorSet, MultiaddrWithPeerId, PublicKey, TaskOutput};
use sp_consensus::SyncOracle;
use sp_consensus_subspace::digests::PreDigest;
use sp_domains::GenerateGenesisStateRoot;
use sp_runtime::DigestItem;
use subspace_core_primitives::{HistorySize, PieceIndexHash, SegmentIndex};
use subspace_farmer::node_client::NodeClient;
use subspace_farmer::piece_cache::PieceCache;
use subspace_farmer_components::FarmerProtocolInfo;
use subspace_networking::{
    PieceByHashRequest, PieceByHashResponse, SegmentHeaderRequest, SegmentHeaderResponse,
};
use subspace_runtime::RuntimeApi;
use subspace_runtime_primitives::opaque::{Block as RuntimeBlock, Header};
use subspace_service::SubspaceConfiguration;
use tokio::sync::oneshot;

mod builder;
pub mod chain_spec;
mod domains;

pub use builder::*;
pub use domains::builder::{DomainConfig, DomainConfigBuilder};
pub use domains::domain::Domain;
use domains::domain_genesis_block_builder::DomainGenesisBlockBuilder;
pub use subspace_runtime::RuntimeEvent as Event;
use tracing::Instrument;

use crate::domains::builder::ConsensusNodeLink;

/// Events from subspace pallet
pub type SubspaceEvent = pallet_subspace::Event<subspace_runtime::Runtime>;

/// Events from subspace pallet
pub type RewardsEvent = pallet_rewards::Event<subspace_runtime::Runtime>;

impl<F: Farmer + 'static> Config<F> {
    /// Start a node with supplied parameters
    pub async fn build(
        self,
        directory: impl AsRef<Path>,
        chain_spec: ChainSpec,
        farmer_total_space_pledged: usize,
    ) -> anyhow::Result<Node<F>> {
        let Self {
            base,
            piece_cache_size,
            mut dsn,
            sync_from_dsn,
            storage_monitor,
            enable_subspace_block_relay,
            ..
        } = self;
        let base = base.configuration(directory.as_ref(), chain_spec.clone()).await;
        let name = base.network.node_name.clone();
        let database_source = base.database.clone();

        let construct_domain_genesis_block_builder =
            |backend, executor| -> Arc<dyn GenerateGenesisStateRoot> {
                Arc::new(DomainGenesisBlockBuilder::new(backend, executor))
            };
        let partial_components = subspace_service::new_partial::<
            F::Table,
            RuntimeApi,
            ExecutorDispatch,
        >(&base, Some(&construct_domain_genesis_block_builder))
        .context("Failed to build a partial subspace node")?;

        let (subspace_networking, dsn, mut runner) = {
            let keypair = {
                let keypair = base
                    .network
                    .node_key
                    .clone()
                    .into_keypair()
                    .context("Failed to convert network keypair")?
                    .to_protobuf_encoding()
                    .context("Failed to convert network keypair")?;

                subspace_networking::libp2p::identity::Keypair::from_protobuf_encoding(&keypair)
                    .expect("Address is correct")
            };

            let chain_spec_boot_nodes = base
                .chain_spec
                .properties()
                .get("dsnBootstrapNodes")
                .cloned()
                .map(serde_json::from_value::<Vec<_>>)
                .transpose()
                .context("Failed to decode DSN bootsrap nodes")?
                .unwrap_or_default();

            tracing::trace!("Subspace networking starting.");

            dsn.boot_nodes.extend(chain_spec_boot_nodes);
            let bootstrap_nodes =
                dsn.boot_nodes.clone().into_iter().map(Into::into).collect::<Vec<_>>();

            let segment_header_store = partial_components.other.2.clone();

            let (dsn, runner) = dsn.build_dsn(DsnOptions {
                client: partial_components.client.clone(),
                node_name: name.clone(),
                archived_segment_notification_stream: partial_components
                    .other
                    .1
                    .archived_segment_notification_stream()
                    .subscribe(),
                piece_cache_size: *piece_cache_size,
                keypair,
                base_path: directory.as_ref().to_path_buf(),
                get_piece_by_hash: get_piece_by_hash::<F>,
                get_segment_header_by_segment_indexes,
                farmer_total_space_pledged,
                segment_header_store,
            })?;

            tracing::debug!("Subspace networking initialized: Node ID is {}", dsn.node.id());

            (
                subspace_service::SubspaceNetworking::Reuse {
                    node: dsn.node.clone(),
                    bootstrap_nodes,
                },
                dsn,
                runner,
            )
        };

        // Default value are used for many of parameters
        let configuration = SubspaceConfiguration {
            base,
            force_new_slot_notifications: false,
            subspace_networking,
            sync_from_dsn,
            enable_subspace_block_relay,
        };

        let node_runner_future = subspace_farmer::utils::run_future_in_dedicated_thread(
            Box::pin({
                async move {
                    runner.run().await;
                    tracing::error!("Exited from node runner future");
                }
            }),
            format!("subspace-sdk-networking-{name}"),
        )
        .context("Failed to run node runner future")?;

        let slot_proportion = sc_consensus_slots::SlotProportion::new(3f32 / 4f32);
        let full_client = subspace_service::new_full::<F::Table, _, _, _>(
            configuration,
            partial_components,
            true,
            slot_proportion,
        )
        .await
        .context("Failed to build a full subspace node")?;

        let NewFull {
            mut task_manager,
            client,
            rpc_handlers,
            network_starter,
            sync_service,
            network_service,

            backend: _,
            select_chain,
            reward_signing_notification_stream: _,
            archived_segment_notification_stream: _,
            transaction_pool: _,
            block_importing_notification_stream,
            new_slot_notification_stream,
        } = full_client;

        if let Some(storage_monitor) = storage_monitor {
            sc_storage_monitor::StorageMonitorService::try_spawn(
                storage_monitor.into(),
                database_source,
                &task_manager.spawn_essential_handle(),
            )
            .context("Failed to start storage monitor")?;
        }

        let mut destructors = DestructorSet::new("node-destructors");

        let (task_manager_drop_sender, task_manager_drop_receiver) = oneshot::channel();
        let (task_manager_result_sender, task_manager_result_receiver) = oneshot::channel();
        let task_manager_join_handle = sdk_utils::task_spawn(
            format!("subspace-sdk-node-{name}-task-manager"),
            {
                async move {
                    futures::select! {
                        _ = task_manager_drop_receiver.fuse() => {
                            let _ = task_manager_result_sender.send(Ok(TaskOutput::Cancelled("received drop signal for task manager".into())));
                        },
                        result = task_manager.future().fuse() => {
                            let _ = task_manager_result_sender.send(result.map_err(anyhow::Error::new).map(TaskOutput::Value));
                        }
                        _ = node_runner_future.fuse() => {
                            let _ = task_manager_result_sender.send(Ok(TaskOutput::Value(())));
                        }
                    }
                }
            },
        );

        destructors.add_async_destructor({
            async move {
                let _ = task_manager_drop_sender.send(());
                task_manager_join_handle.await.expect("joining should not fail; qed");
            }
        })?;

        let mut maybe_domain = None;
        if let Some(domain_config) = self.domain {
            let base_directory = directory.as_ref().to_owned().clone();

            let domain = domain_config
                .build(
                    base_directory,
                    ConsensusNodeLink {
                        consensus_client: client.clone(),
                        block_importing_notification_stream: block_importing_notification_stream
                            .clone(),
                        new_slot_notification_stream: new_slot_notification_stream.clone(),
                        consensus_network_service: network_service.clone(),
                        consensus_sync_service: sync_service.clone(),
                        select_chain: select_chain.clone(),
                    },
                )
                .await?;

            maybe_domain = Some(domain);
        }

        let rpc_handle = sdk_utils::Rpc::new(&rpc_handlers);
        network_starter.start_network();

        // Disable proper exit for now. Because RPC server looses waker and can't exit
        // in background.
        //
        // drop_collection.defer(move || {
        //     const BUSY_WAIT_INTERVAL: Duration = Duration::from_millis(100);
        //
        //     // Busy wait till backend exits
        //     // TODO: is it the only wait to check that substrate node exited?
        //     while Arc::strong_count(&backend) != 1 {
        //         std::thread::sleep(BUSY_WAIT_INTERVAL);
        //     }
        // });

        tracing::debug!("Started node");

        Ok(Node {
            client,
            network_service,
            sync_service,
            name,
            rpc_handle,
            dsn,
            _destructors: destructors,
            _farmer: Default::default(),
            task_manager_result_receiver,
            maybe_domain,
        })
    }
}

/// Executor dispatch for subspace runtime
pub struct ExecutorDispatch;

impl sc_executor::NativeExecutionDispatch for ExecutorDispatch {
    // /// Only enable the benchmarking host functions when we actually want to
    // benchmark. #[cfg(feature = "runtime-benchmarks")]
    // type ExtendHostFunctions = (
    //     frame_benchmarking::benchmarking::HostFunctions,
    //     sp_consensus_subspace::consensus::HostFunctions,
    // )
    // /// Otherwise we only use the default Substrate host functions.
    // #[cfg(not(feature = "runtime-benchmarks"))]
    type ExtendHostFunctions =
        (sp_consensus_subspace::consensus::HostFunctions, sp_domains::domain::HostFunctions);

    fn dispatch(method: &str, data: &[u8]) -> Option<Vec<u8>> {
        subspace_runtime::api::dispatch(method, data)
    }

    fn native_version() -> sc_executor::NativeVersion {
        subspace_runtime::native_version()
    }
}

/// Chain spec for subspace node
pub type ChainSpec = chain_spec::ChainSpec;
pub(crate) type FullClient =
    subspace_service::FullClient<subspace_runtime::RuntimeApi, ExecutorDispatch>;
pub(crate) type NewFull = subspace_service::NewFull<
    FullClient,
    subspace_service::tx_pre_validator::ConsensusChainTxPreValidator<
        RuntimeBlock,
        FullClient,
        subspace_service::FraudProofVerifier<RuntimeApi, ExecutorDispatch>,
    >,
>;

/// Node structure
#[derive(Derivative)]
#[derivative(Debug)]
#[must_use = "Node should be closed"]
pub struct Node<F: Farmer> {
    #[derivative(Debug = "ignore")]
    client: Arc<FullClient>,
    #[derivative(Debug = "ignore")]
    sync_service: Arc<sc_network_sync::service::chain_sync::SyncingService<RuntimeBlock>>,
    #[derivative(Debug = "ignore")]
    network_service: Arc<NetworkService<RuntimeBlock, Hash>>,
    rpc_handle: sdk_utils::Rpc,
    name: String,
    dsn: DsnShared<FullClient>,
    #[derivative(Debug = "ignore")]
    _destructors: DestructorSet,
    #[derivative(Debug = "ignore")]
    _farmer: std::marker::PhantomData<F>,
    #[derivative(Debug = "ignore")]
    task_manager_result_receiver: oneshot::Receiver<anyhow::Result<TaskOutput<(), String>>>,
    #[derivative(Debug = "ignore")]
    maybe_domain: Option<Domain>,
}

impl<F: Farmer> sdk_traits::Node for Node<F> {
    type Client = FullClient;
    type Rpc = sdk_utils::Rpc;
    type Table = F::Table;

    fn name(&self) -> &str {
        &self.name
    }

    fn dsn(&self) -> &DsnShared<Self::Client> {
        &self.dsn
    }

    fn rpc(&self) -> &Self::Rpc {
        &self.rpc_handle
    }
}

/// Hash type
pub type Hash = <subspace_runtime::Runtime as frame_system::Config>::Hash;
/// Block number
pub type BlockNumber = <subspace_runtime::Runtime as frame_system::Config>::BlockNumber;

/// Chain info
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ChainInfo {
    /// Genesis hash of chain
    pub genesis_hash: Hash,
}

/// Node state info
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Info {
    /// Chain info
    pub chain: ChainInfo,
    /// Best block hash and number
    pub best_block: (Hash, BlockNumber),
    /// Finalized block hash and number
    pub finalized_block: (Hash, BlockNumber),
    /// Block gap which we need to sync
    pub block_gap: Option<std::ops::Range<BlockNumber>>,
    /// Runtime version
    pub version: sp_version::RuntimeVersion,
    /// Node telemetry name
    pub name: String,
    /// Number of peers connected to our node
    pub connected_peers: u64,
    /// Number of nodes that we know of but that we're not connected to
    pub not_connected_peers: u64,
    /// Total number of pieces stored on chain
    pub history_size: HistorySize,
}

/// New block notification
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct BlockHeader {
    /// Block hash
    pub hash: Hash,
    /// Block number
    pub number: BlockNumber,
    /// Parent block hash
    pub parent_hash: Hash,
    /// Block state root
    pub state_root: Hash,
    /// Extrinsics root
    pub extrinsics_root: Hash,
    /// Block pre digest
    pub pre_digest: Option<PreDigest<PublicKey, PublicKey>>,
}

impl From<Header> for BlockHeader {
    fn from(header: Header) -> Self {
        let hash = header.hash();
        let Header { number, parent_hash, state_root, extrinsics_root, digest } = header;
        let pre_digest = digest
            .log(|it| if let DigestItem::PreRuntime(_, digest) = it { Some(digest) } else { None })
            .map(|pre_digest| {
                parity_scale_codec::Decode::decode(&mut pre_digest.as_ref())
                    .expect("Pre digest is always scale encoded")
            });
        Self { hash, number, parent_hash, state_root, extrinsics_root, pre_digest }
    }
}

/// Syncing status
#[derive(Clone, Copy, Debug)]
pub enum SyncStatus {
    /// Importing some block
    Importing,
    /// Downloading some block
    Downloading,
}

/// Current syncing progress
#[derive(Clone, Copy, Debug)]
pub struct SyncingProgress {
    /// Imported this much blocks
    pub at: BlockNumber,
    /// Number of total blocks
    pub target: BlockNumber,
    /// Current syncing status
    pub status: SyncStatus,
}

#[pin_project::pin_project]
struct SyncingProgressStream<S> {
    #[pin]
    inner: S,
    at: BlockNumber,
    target: BlockNumber,
}

impl<E, S: Stream<Item = Result<SyncingProgress, E>>> Stream for SyncingProgressStream<S> {
    type Item = Result<SyncingProgress, E>;

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        let this = self.project();
        let next = this.inner.poll_next(cx);
        if let std::task::Poll::Ready(Some(Ok(SyncingProgress { at, target, .. }))) = next {
            *this.at = at;
            *this.target = target;
        }
        next
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.at as _, Some(self.target as _))
    }
}

impl<F: Farmer + 'static> Node<F> {
    /// New node builder
    pub fn builder() -> Builder<F> {
        Builder::new()
    }

    /// Development configuration
    pub fn dev() -> Builder<F> {
        Builder::dev()
    }

    /// Gemini 3e configuration
    pub fn gemini_3e() -> Builder<F> {
        Builder::gemini_3e()
    }

    /// Devnet configuration
    pub fn devnet() -> Builder<F> {
        Builder::devnet()
    }

    /// Get listening addresses of the node
    pub async fn listen_addresses(&self) -> anyhow::Result<Vec<MultiaddrWithPeerId>> {
        let peer_id = self.network_service.local_peer_id();
        self.network_service
            .network_state()
            .await
            .map(|state| {
                state
                    .listened_addresses
                    .into_iter()
                    .map(|multiaddr| MultiaddrWithPeerId::new(multiaddr, peer_id))
                    .collect()
            })
            .map_err(|()| anyhow::anyhow!("Network worker exited"))
    }

    /// Get listening addresses of the node
    pub async fn dsn_listen_addresses(&self) -> anyhow::Result<Vec<MultiaddrWithPeerId>> {
        let peer_id =
            self.dsn.node.id().to_string().parse().expect("Conversion between 2 libp2p versions");
        Ok(self
            .dsn
            .node
            .listeners()
            .into_iter()
            .map(|multiaddr| MultiaddrWithPeerId::new(multiaddr, peer_id))
            .collect())
    }

    /// Subscribe for node syncing progress
    pub async fn subscribe_syncing_progress(
        &self,
    ) -> anyhow::Result<impl Stream<Item = anyhow::Result<SyncingProgress>> + Send + Unpin + 'static>
    {
        const CHECK_SYNCED_EVERY: Duration = Duration::from_millis(100);
        let check_offline_backoff = backoff::ExponentialBackoffBuilder::new()
            .with_max_elapsed_time(Some(Duration::from_secs(60)))
            .build();
        let check_synced_backoff = backoff::ExponentialBackoffBuilder::new()
            .with_initial_interval(Duration::from_secs(1))
            .with_max_elapsed_time(Some(Duration::from_secs(10)))
            .build();

        backoff::future::retry(check_offline_backoff, || {
            futures::future::ready(if self.sync_service.is_offline() {
                Err(backoff::Error::transient(()))
            } else {
                Ok(())
            })
        })
        .await
        .map_err(|_| anyhow::anyhow!("Failed to connect to the network"))?;

        let (sender, receiver) = tokio::sync::mpsc::channel(10);
        let inner = tokio_stream::wrappers::ReceiverStream::new(receiver);

        let result = backoff::future::retry(check_synced_backoff.clone(), || {
            self.sync_service.status().map(|result| match result.map(|status| status.state) {
                Ok(SyncState::Importing { target }) => Ok((target, SyncStatus::Importing)),
                Ok(SyncState::Downloading { target }) => Ok((target, SyncStatus::Downloading)),
                _ if self.sync_service.is_offline() =>
                    Err(backoff::Error::transient(Some(anyhow::anyhow!("Node went offline")))),
                Err(()) => Err(backoff::Error::transient(Some(anyhow::anyhow!(
                    "Failed to fetch networking status"
                )))),
                Ok(SyncState::Idle | SyncState::Pending) => Err(backoff::Error::transient(None)),
            })
        })
        .await;

        let (target, status) = match result {
            Ok(result) => result,
            Err(Some(err)) => return Err(err),
            // We are idle for quite some time
            Err(None) => return Ok(SyncingProgressStream { inner, at: 0, target: 0 }),
        };

        let at = self.client.chain_info().best_number;
        sender
            .send(Ok(SyncingProgress { target, at, status }))
            .await
            .expect("We are holding receiver, so it will never panic");

        tokio::spawn({
            let sync = Arc::clone(&self.sync_service);
            let client = Arc::clone(&self.client);
            async move {
                loop {
                    tokio::time::sleep(CHECK_SYNCED_EVERY).await;

                    let result = backoff::future::retry(check_synced_backoff.clone(), || {
                        sync.status().map(|result| match result.map(|status| status.state) {
                            Ok(SyncState::Importing { target }) =>
                                Ok(Ok((target, SyncStatus::Importing))),
                            Ok(SyncState::Downloading { target }) =>
                                Ok(Ok((target, SyncStatus::Downloading))),
                            Err(()) =>
                                Ok(Err(anyhow::anyhow!("Failed to fetch networking status"))),
                            Ok(SyncState::Idle | SyncState::Pending) =>
                                Err(backoff::Error::transient(())),
                        })
                    })
                    .await;
                    let Ok(result) = result else { break };

                    if sender
                        .send(result.map(|(target, status)| SyncingProgress {
                            target,
                            at: client.chain_info().best_number,
                            status,
                        }))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            }
        });

        Ok(SyncingProgressStream { inner, at, target })
    }

    /// Wait till the end of node syncing
    pub async fn sync(&self) -> anyhow::Result<()> {
        self.subscribe_syncing_progress().await?.for_each(|_| async move {}).await;
        Ok(())
    }

    /// Leaves the network and gracefully shuts down
    pub async fn close(self) -> anyhow::Result<()> {
        if let Some(domain) = self.maybe_domain {
            domain.close().await?;
        }
        self._destructors.async_drop().await?;
        let output = self.task_manager_result_receiver.await??;
        match output {
            TaskOutput::Value(_) => {}
            TaskOutput::Cancelled(reason) => {
                tracing::warn!("node task manager was cancelled due to reason: {}", reason);
            }
        }
        Ok(())
    }

    /// Tells if the node was closed
    pub async fn is_closed(&self) -> bool {
        self._destructors.already_ran()
    }

    /// Runs `.close()` and also wipes node's state
    pub async fn wipe(path: impl AsRef<Path>) -> io::Result<()> {
        tokio::fs::remove_dir_all(path).await
    }

    /// Get node info
    pub async fn get_info(&self) -> anyhow::Result<Info> {
        let NetworkState { connected_peers, not_connected_peers, .. } = self
            .network_service
            .network_state()
            .await
            .map_err(|()| anyhow::anyhow!("Failed to fetch node info: node already exited"))?;
        let sp_blockchain::Info {
            best_hash,
            best_number,
            genesis_hash,
            finalized_hash,
            finalized_number,
            block_gap,
            ..
        } = self.client.chain_info();
        let version = self.rpc_handle.runtime_version(Some(best_hash)).await?;
        let FarmerProtocolInfo { history_size, .. } =
            self.rpc_handle.farmer_app_info().await.map_err(anyhow::Error::msg)?.protocol_info;
        Ok(Info {
            chain: ChainInfo { genesis_hash },
            best_block: (best_hash, best_number),
            finalized_block: (finalized_hash, finalized_number),
            block_gap: block_gap.map(|(from, to)| from..to),
            version,
            name: self.name.clone(),
            connected_peers: connected_peers.len() as u64,
            not_connected_peers: not_connected_peers.len() as u64,
            history_size,
        })
    }

    /// Get block hash by block number
    pub fn block_hash(&self, number: BlockNumber) -> anyhow::Result<Option<Hash>> {
        use sc_client_api::client::BlockBackend;

        self.client.block_hash(number).context("Failed to get primary node block hash by number")
    }

    /// Get block header by hash
    pub fn block_header(&self, hash: Hash) -> anyhow::Result<Option<BlockHeader>> {
        self.client
            .header(hash)
            .context("Failed to get primary node block hash by number")
            .map(|opt| opt.map(Into::into))
    }

    /// Subscribe to new heads imported
    pub async fn subscribe_new_heads(
        &self,
    ) -> anyhow::Result<impl Stream<Item = BlockHeader> + Send + Sync + Unpin + 'static> {
        Ok(self
            .rpc_handle
            .subscribe_new_heads::<subspace_runtime::Runtime>()
            .await
            .context("Failed to subscribe to new blocks")?
            .map(Into::into))
    }

    /// Subscribe to finalized heads
    pub async fn subscribe_finalized_heads(
        &self,
    ) -> anyhow::Result<impl Stream<Item = BlockHeader> + Send + Sync + Unpin + 'static> {
        Ok(self
            .rpc_handle
            .subscribe_finalized_heads::<subspace_runtime::Runtime>()
            .await
            .context("Failed to subscribe to finalized blocks")?
            .map(Into::into))
    }

    /// Get events at some block or at tip of the chain
    pub async fn get_events(&self, block: Option<Hash>) -> anyhow::Result<Vec<Event>> {
        Ok(self
            .rpc_handle
            .get_events::<subspace_runtime::Runtime>(block)
            .await?
            .into_iter()
            .map(|event_record| event_record.event)
            .collect())
    }
}

const SEGMENT_HEADERS_NUMBER_LIMIT: u64 = 1000;

fn get_segment_header_by_segment_indexes(
    req: &SegmentHeaderRequest,
    segment_headers_store: &SegmentHeadersStore<impl sc_client_api::AuxStore>,
) -> Option<SegmentHeaderResponse> {
    let segment_indexes = match req {
        SegmentHeaderRequest::SegmentIndexes { segment_indexes } => segment_indexes.clone(),
        SegmentHeaderRequest::LastSegmentHeaders { segment_header_number } => {
            let mut segment_headers_limit = *segment_header_number;
            if *segment_header_number > SEGMENT_HEADERS_NUMBER_LIMIT {
                tracing::debug!(%segment_header_number, "Segment header number exceeded the limit.");

                segment_headers_limit = SEGMENT_HEADERS_NUMBER_LIMIT;
            }

            match segment_headers_store.max_segment_index() {
                Some(max_segment_index) => {
                    // Several last segment indexes
                    (SegmentIndex::ZERO..=max_segment_index)
                        .rev()
                        .take(segment_headers_limit as usize)
                        .collect::<Vec<_>>()
                }
                None => {
                    // Nothing yet
                    Vec::new()
                }
            }
        }
    };

    let maybe_segment_headers = segment_indexes
        .iter()
        .map(|segment_index| segment_headers_store.get_segment_header(*segment_index))
        .collect::<Option<Vec<subspace_core_primitives::SegmentHeader>>>();

    match maybe_segment_headers {
        Some(segment_headers) => Some(SegmentHeaderResponse { segment_headers }),
        None => {
            tracing::error!("Segment header collection contained empty segment headers.");
            None
        }
    }
}

fn get_piece_by_hash<F: Farmer>(
    &PieceByHashRequest { piece_index_hash }: &PieceByHashRequest,
    weak_readers_and_pieces: std::sync::Weak<
        parking_lot::Mutex<Option<subspace_farmer::utils::readers_and_pieces::ReadersAndPieces>>,
    >,
    farmer_piece_cache: Arc<tokio::sync::RwLock<Option<PieceCache>>>,
    piece_cache: NodePieceCache<impl sc_client_api::AuxStore>,
) -> impl std::future::Future<Output = Option<PieceByHashResponse>> {
    async move {
        match node_get_piece_by_hash(piece_index_hash, &piece_cache) {
            Some(PieceByHashResponse { piece: None }) | None => (),
            result => return result,
        }

        if let Some(farmer_piece_cache) = farmer_piece_cache.read().await.as_ref() {
            let piece = F::get_piece_by_hash(
                piece_index_hash,
                farmer_piece_cache,
                &weak_readers_and_pieces,
            )
            .await;
            Some(PieceByHashResponse { piece })
        } else {
            None
        }
    }
    .in_current_span()
}

fn node_get_piece_by_hash(
    piece_index_hash: PieceIndexHash,
    piece_cache: &NodePieceCache<impl sc_client_api::AuxStore>,
) -> Option<PieceByHashResponse> {
    let result = match piece_cache.get_piece(piece_index_hash) {
        Ok(maybe_piece) => maybe_piece,
        Err(error) => {
            tracing::error!(?piece_index_hash, %error, "Failed to get piece from cache");
            None
        }
    };

    Some(PieceByHashResponse { piece: result })
}

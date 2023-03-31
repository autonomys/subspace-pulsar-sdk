use std::io;
use std::num::NonZeroU64;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use derivative::Derivative;
use futures::channel::{mpsc, oneshot};
use futures::{FutureExt, SinkExt, Stream, StreamExt};
use sc_consensus_subspace_rpc::SegmentHeaderProvider;
use sc_network::network_state::NetworkState;
use sc_network::{NetworkService, NetworkStateInfo, NetworkStatusProvider, SyncState};
use sc_network_common::config::MultiaddrWithPeerId;
use sc_rpc_api::state::StateApiClient;
use sp_consensus::SyncOracle;
use sp_core::H256;
use subspace_core_primitives::{PieceIndexHash, SegmentIndex};
use subspace_farmer::node_client::NodeClient;
use subspace_farmer::utils::parity_db_store::ParityDbStore;
use subspace_farmer::utils::readers_and_pieces::ReadersAndPieces;
use subspace_farmer_components::piece_caching::PieceMemoryCache;
use subspace_farmer_components::FarmerProtocolInfo;
use subspace_networking::{PieceByHashResponse, SegmentHeaderRequest, SegmentHeaderResponse};
use subspace_runtime::RuntimeApi;
use subspace_runtime_primitives::opaque::{Block as RuntimeBlock, Header};
use subspace_service::segment_headers::SegmentHeaderCache;
use subspace_service::SubspaceConfiguration;
use substrate::Base;
use tracing_futures::Instrument;

use crate::dsn::provider_storage_utils::MaybeProviderStorage;
use crate::dsn::{FarmerProviderStorage, NodePieceCache};
use crate::utils::DropCollection;

mod builder;
pub mod chain_spec;
pub mod domains;
mod farmer_rpc_client;
mod substrate;

pub use builder::*;
pub use domains::{ConfigBuilder as SystemDomainBuilder, SystemDomainNode};
pub use substrate::*;

pub use crate::dsn::builder::*;

impl Config {
    /// Start a node with supplied parameters
    pub async fn build(
        self,
        directory: impl AsRef<Path>,
        chain_spec: ChainSpec,
    ) -> anyhow::Result<Node> {
        let Self {
            base,
            piece_cache_size,
            mut dsn,
            system_domain,
            segment_publish_concurrency: SegmentPublishConcurrency(segment_publish_concurrency),
            sync_from_dsn,
            storage_monitor,
        } = self;
        let base = base.configuration(directory.as_ref(), chain_spec.clone()).await;
        let name = base.network.node_name.clone();
        let database_source = base.database.clone();

        let partial_components =
            subspace_service::new_partial::<RuntimeApi, ExecutorDispatch>(&base)
                .context("Failed to build a partial subspace node")?;
        let farmer_readers_and_pieces = Arc::new(parking_lot::Mutex::new(None));
        let farmer_piece_store = Arc::new(tokio::sync::Mutex::new(None));
        let farmer_provider_storage = MaybeProviderStorage::none();
        let piece_memory_cache = PieceMemoryCache::default();

        let (subspace_networking, (node, mut node_runner, piece_cache)) = {
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

            let piece_cache = NodePieceCache::new(
                partial_components.client.clone(),
                piece_cache_size.as_u64() / subspace_core_primitives::Piece::SIZE as u64,
                subspace_networking::peer_id(&keypair),
            );

            // Start before archiver below, so we don't have potential race condition and
            // miss pieces
            tokio::task::Builder::new()
                .name(format!("subspace-sdk-node-{name}-piece-caching").as_ref())
                .spawn({
                    let mut piece_cache = piece_cache.clone();
                    let mut archived_segment_notification_stream = partial_components
                        .other
                        .1
                        .archived_segment_notification_stream()
                        .subscribe();

                    async move {
                        while let Some(archived_segment_notification) =
                            archived_segment_notification_stream.next().await
                        {
                            let segment_index = archived_segment_notification
                                .archived_segment
                                .segment_header
                                .segment_index();
                            if let Err(error) = piece_cache.add_pieces(
                                segment_index.first_piece_index(),
                                &archived_segment_notification.archived_segment.pieces,
                            ) {
                                tracing::error!(
                                    %segment_index,
                                    %error,
                                    "Failed to store pieces for segment in cache"
                                );
                            }
                        }
                    }
                })?;

            let chain_spec_boot_nodes = base
                .chain_spec
                .properties()
                .get("dsnBootstrapNodes")
                .cloned()
                .map(serde_json::from_value::<Vec<_>>)
                .transpose()
                .context("Failed to decode DSN bootsrap nodes")?
                .unwrap_or_default();

            let (node, node_runner, bootstrap_nodes) = {
                tracing::trace!("Subspace networking starting.");

                dsn.boot_nodes.extend(chain_spec_boot_nodes);
                let bootstrap_nodes = dsn
                    .boot_nodes
                    .clone()
                    .into_iter()
                    .map(|a| {
                        a.to_string()
                            .parse()
                            .expect("Convertion between 2 libp2p version. Never panics")
                    })
                    .collect::<Vec<_>>();

                let networking_config = dsn.build_config(DsnOptions {
                    segment_header_cache: SegmentHeaderCache::new(
                        partial_components.client.clone(),
                    ),
                    keypair,
                    base_path: directory.as_ref().to_path_buf(),
                    piece_cache: piece_cache.clone(),
                    farmer_piece_store: farmer_piece_store.clone(),
                    farmer_provider_storage: farmer_provider_storage.clone(),
                    farmer_readers_and_pieces: farmer_readers_and_pieces.clone(),
                    piece_memory_cache: piece_memory_cache.clone(),
                })?;

                subspace_networking::create(networking_config)
                    .map(|(a, b)| (a, b, bootstrap_nodes))?
            };

            tracing::debug!("Subspace networking initialized: Node ID is {}", node.id());

            (
                subspace_service::SubspaceNetworking::Reuse { node: node.clone(), bootstrap_nodes },
                (node, node_runner, piece_cache),
            )
        };

        let mut drop_collection = DropCollection::new();
        let on_new_listener = node.on_new_listener(Arc::new({
            let node = node.clone();

            move |address| {
                tracing::info!(
                    "DSN listening on {}",
                    address.clone().with(subspace_networking::libp2p::multiaddr::Protocol::P2p(
                        node.id().into()
                    ))
                );
            }
        }));
        drop_collection.push(on_new_listener);

        // Default value are used for many of parameters
        let configuration = SubspaceConfiguration {
            base,
            force_new_slot_notifications: false,
            segment_publish_concurrency,
            subspace_networking,
            sync_from_dsn,
        };

        let node_runner_future = subspace_farmer::utils::run_future_in_dedicated_thread(
            Box::pin(async move {
                node_runner.run().await;
                tracing::error!("Exited from node runner future");
            }),
            format!("subspace-sdk-networking-{name}"),
        )
        .context("Failed to run node runner future")?;

        let slot_proportion = sc_consensus_slots::SlotProportion::new(2f32 / 3f32);
        let mut full_client =
            subspace_service::new_full(configuration, partial_components, true, slot_proportion)
                .await
                .context("Failed to build a full subspace node")?;

        if let Some(storage_monitor) = storage_monitor {
            sc_storage_monitor::StorageMonitorService::try_spawn(
                storage_monitor.into(),
                database_source,
                &full_client.task_manager.spawn_essential_handle(),
            )
            .context("Failed to start storage monitor")?;
        }

        let system_domain = if let Some(config) = system_domain {
            use sc_service::ChainSpecExtension;
            let span = tracing::info_span!("SystemDomain");

            let system_domain_spec = chain_spec
                .extensions()
                .get_any(std::any::TypeId::of::<domains::ChainSpec>())
                .downcast_ref()
                .cloned()
                .ok_or_else(|| {
                    anyhow::anyhow!("Primary chain spec must contain system domain chain spec")
                })?;

            SystemDomainNode::new(
                config,
                directory.as_ref().join("domains"),
                system_domain_spec,
                &mut full_client,
            )
            .instrument(span)
            .await
            .map(Some)?
        } else {
            None
        };

        let NewFull {
            mut task_manager,
            client,
            rpc_handlers,
            network_starter,
            network,
            backend,

            select_chain: _,
            reward_signing_notification_stream: _,
            archived_segment_notification_stream: _,
            transaction_pool: _,
            imported_block_notification_stream: _,
            new_slot_notification_stream: _,
        } = full_client;

        let rpc_handle = crate::utils::Rpc::new(&rpc_handlers);
        network_starter.start_network();
        let (stop_sender, mut stop_receiver) = mpsc::channel::<oneshot::Sender<()>>(1);

        tokio::task::Builder::new()
            .name(format!("subspace-sdk-node-{name}-task-manager").as_ref())
            .spawn(async move {
                let opt_stop_sender = async move {
                    futures::select! {
                        opt_sender = stop_receiver.next() => opt_sender,
                        result = task_manager.future().fuse() => {
                            result.expect("Task from manager paniced");
                            None
                        }
                        _ = node_runner_future.fuse() => None,
                    }
                }
                .await;
                opt_stop_sender.map(|stop_sender| stop_sender.send(()));
            })?;

        drop_collection.defer(move || {
            const BUSY_WAIT_INTERVAL: Duration = Duration::from_millis(100);

            // Busy wait till backend exits
            // TODO: is it the only wait to check that substrate node exited?
            while Arc::strong_count(&backend) != 1 {
                std::thread::sleep(BUSY_WAIT_INTERVAL);
            }
        });

        tracing::debug!("Started node");

        Ok(Node {
            client,
            system_domain,
            network,
            name,
            rpc_handle,
            dsn_node: node,
            stop_sender,
            farmer_readers_and_pieces,
            farmer_piece_store,
            farmer_provider_storage,
            piece_cache,
            piece_memory_cache,
            _drop_at_exit: drop_collection,
        })
    }
}

/// Executor dispatch for subspace runtime
pub(crate) struct ExecutorDispatch;

impl sc_executor::NativeExecutionDispatch for ExecutorDispatch {
    // /// Only enable the benchmarking host functions when we actually want to
    // benchmark. #[cfg(feature = "runtime-benchmarks")]
    // type ExtendHostFunctions = (
    //     frame_benchmarking::benchmarking::HostFunctions,
    //     sp_consensus_subspace::consensus::HostFunctions,
    // )
    // /// Otherwise we only use the default Substrate host functions.
    // #[cfg(not(feature = "runtime-benchmarks"))]
    type ExtendHostFunctions = sp_consensus_subspace::consensus::HostFunctions;

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
    subspace_service::tx_pre_validator::PrimaryChainTxPreValidator<
        RuntimeBlock,
        FullClient,
        subspace_service::FraudProofVerifier<RuntimeApi, ExecutorDispatch>,
        subspace_transaction_pool::bundle_validator::BundleValidator<RuntimeBlock, FullClient>,
    >,
>;

/// Node structure
#[derive(Derivative)]
#[derivative(Debug)]
#[must_use = "Node should be closed"]
pub struct Node {
    system_domain: Option<SystemDomainNode>,
    #[derivative(Debug = "ignore")]
    client: Arc<FullClient>,
    #[derivative(Debug = "ignore")]
    network: Arc<NetworkService<RuntimeBlock, Hash>>,
    pub(crate) rpc_handle: crate::utils::Rpc,
    pub(crate) stop_sender: mpsc::Sender<oneshot::Sender<()>>,
    pub(crate) dsn_node: subspace_networking::Node,
    pub(crate) name: String,
    pub(crate) farmer_readers_and_pieces: Arc<parking_lot::Mutex<Option<ReadersAndPieces>>>,
    #[derivative(Debug = "ignore")]
    pub(crate) farmer_piece_store: Arc<
        tokio::sync::Mutex<
            Option<
                ParityDbStore<
                    subspace_networking::libp2p::kad::record::Key,
                    subspace_core_primitives::Piece,
                >,
            >,
        >,
    >,
    pub(crate) farmer_provider_storage: MaybeProviderStorage<FarmerProviderStorage>,
    #[derivative(Debug = "ignore")]
    pub(crate) piece_cache: NodePieceCache<FullClient>,
    #[derivative(Debug = "ignore")]
    pub(crate) piece_memory_cache: PieceMemoryCache,

    #[derivative(Debug = "ignore")]
    _drop_at_exit: DropCollection,
}

static_assertions::assert_impl_all!(Node: Send, Sync);
static_assertions::assert_impl_all!(SystemDomainNode: Send, Sync);

/// Hash type
pub type Hash = H256;
/// Block number
pub type BlockNumber = u32;

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
    pub total_pieces: NonZeroU64,
}

/// New block notification
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct BlockNotification {
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
}

impl From<Header> for BlockNotification {
    fn from(header: Header) -> Self {
        let hash = header.hash();
        let Header { number, parent_hash, state_root, extrinsics_root, digest: _ } = header;
        Self { hash, number, parent_hash, state_root, extrinsics_root }
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

impl Node {
    /// New node builder
    pub fn builder() -> Builder {
        Builder::new()
    }

    /// Development configuration
    pub fn dev() -> Builder {
        Builder::dev()
    }

    /// Gemini 3d configuration
    pub fn gemini_3d() -> Builder {
        Builder::gemini_3d()
    }

    /// Devnet configuration
    pub fn devnet() -> Builder {
        Builder::devnet()
    }

    /// Get listening addresses of the node
    pub async fn listen_addresses(&self) -> anyhow::Result<Vec<MultiaddrWithPeerId>> {
        let peer_id = self.network.local_peer_id();
        self.network
            .network_state()
            .await
            .map(|state| {
                state
                    .listened_addresses
                    .into_iter()
                    .map(|multiaddr| MultiaddrWithPeerId { multiaddr, peer_id })
                    .collect()
            })
            .map_err(|()| anyhow::anyhow!("Network worker exited"))
    }

    /// Get listening addresses of the node
    pub async fn dsn_listen_addresses(&self) -> anyhow::Result<Vec<MultiaddrWithPeerId>> {
        let peer_id = self.dsn_node.id();
        Ok(self
            .dsn_node
            .listeners()
            .into_iter()
            .map(|mut multiaddr| {
                multiaddr
                    .push(subspace_networking::libp2p::multiaddr::Protocol::P2p(peer_id.into()));
                multiaddr
                    .to_string()
                    .parse()
                    .expect("Convertion between 2 libp2p version. Never panics")
            })
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
            futures::future::ready(if self.network.is_offline() {
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
            self.network.status().map(|result| match result.map(|status| status.sync_state) {
                Ok(SyncState::Importing { target }) => Ok((target, SyncStatus::Importing)),
                Ok(SyncState::Downloading { target }) => Ok((target, SyncStatus::Downloading)),
                _ if self.network.is_offline() =>
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
            let network = Arc::clone(&self.network);
            let client = Arc::clone(&self.client);
            async move {
                loop {
                    tokio::time::sleep(CHECK_SYNCED_EVERY).await;

                    let result = backoff::future::retry(check_synced_backoff.clone(), || {
                        network.status().map(|result| {
                            match result.map(|status| status.sync_state) {
                                Ok(SyncState::Importing { target }) =>
                                    Ok(Ok((target, SyncStatus::Importing))),
                                Ok(SyncState::Downloading { target }) =>
                                    Ok(Ok((target, SyncStatus::Downloading))),
                                Err(()) =>
                                    Ok(Err(anyhow::anyhow!("Failed to fetch networking status"))),
                                Ok(SyncState::Idle | SyncState::Pending) =>
                                    Err(backoff::Error::transient(())),
                            }
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
    pub async fn close(mut self) -> anyhow::Result<()> {
        let (stop_sender, stop_receiver) = oneshot::channel();
        let _ = match self.stop_sender.send(stop_sender).await {
            Err(_) => return Err(anyhow::anyhow!("Node was already closed")),
            Ok(()) => stop_receiver.await,
        };

        Ok(())
    }

    /// Tells if the node was closed
    pub async fn is_closed(&self) -> bool {
        self.stop_sender.is_closed()
    }

    /// Runs `.close()` and also wipes node's state
    pub async fn wipe(path: impl AsRef<Path>) -> io::Result<()> {
        tokio::fs::remove_dir_all(path).await
    }

    /// Returns system domain node if one was setted up
    pub fn system_domain(&self) -> Option<SystemDomainNode> {
        self.system_domain.as_ref().cloned()
    }

    /// Get node info
    pub async fn get_info(&self) -> anyhow::Result<Info> {
        let NetworkState { connected_peers, not_connected_peers, .. } = self
            .network
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
        let FarmerProtocolInfo { total_pieces, .. } =
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
            total_pieces,
        })
    }

    /// Subscribe to new blocks imported
    pub async fn subscribe_new_blocks(
        &self,
    ) -> anyhow::Result<impl Stream<Item = BlockNotification> + Send + Sync + Unpin + 'static> {
        self.rpc_handle.subscribe_new_blocks().await.context("Failed to subscribe to new blocks")
    }
}

const ROOT_BLOCK_NUMBER_LIMIT: u64 = 100;

pub(crate) fn get_segment_header_by_segment_indexes(
    req: &SegmentHeaderRequest,
    segment_header_cache: &SegmentHeaderCache<impl sc_client_api::AuxStore>,
) -> Option<SegmentHeaderResponse> {
    let segment_indexes = match req {
        SegmentHeaderRequest::SegmentIndexes { segment_indexes } => segment_indexes.clone(),
        SegmentHeaderRequest::LastSegmentHeaders { segment_header_number } => {
            let mut block_limit = *segment_header_number;
            if *segment_header_number > ROOT_BLOCK_NUMBER_LIMIT {
                tracing::debug!(%segment_header_number, "Segment header number exceeded the limit.");

                block_limit = ROOT_BLOCK_NUMBER_LIMIT;
            }

            let max_segment_index = segment_header_cache.max_segment_index();

            // several last segment indexes
            (SegmentIndex::ZERO..=max_segment_index)
                .rev()
                .take(block_limit as usize)
                .collect::<Vec<_>>()
        }
    };

    let internal_result = segment_indexes
        .iter()
        .map(|segment_index| segment_header_cache.get_segment_header(*segment_index))
        .collect::<Result<Option<Vec<subspace_core_primitives::SegmentHeader>>, _>>();

    match internal_result {
        Ok(Some(segment_headers)) => Some(SegmentHeaderResponse { segment_headers }),
        Ok(None) => None,
        Err(error) => {
            tracing::error!(%error, "Failed to get segment header from cache");

            None
        }
    }
}

pub(crate) fn get_piece_by_hash(
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

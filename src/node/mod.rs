use std::io;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use derivative::Derivative;
use futures::channel::{mpsc, oneshot};
use futures::{FutureExt, SinkExt, Stream, StreamExt};
use sc_consensus_subspace_rpc::SegmentHeaderProvider;
use sc_network::network_state::NetworkState;
use sc_network::{NetworkService, NetworkStateInfo, SyncState};
use sc_rpc_api::state::StateApiClient;
use sdk_dsn::NodePieceCache;
use sdk_utils::{DropCollection, MultiaddrWithPeerId};
use sp_consensus::SyncOracle;
use sp_consensus_subspace::digests::PreDigest;
use sp_runtime::DigestItem;
use subspace_core_primitives::{HistorySize, PieceIndexHash, SegmentIndex};
use subspace_farmer::node_client::NodeClient;
use subspace_farmer_components::FarmerProtocolInfo;
use subspace_networking::{
    PieceByHashRequest, PieceByHashResponse, SegmentHeaderRequest, SegmentHeaderResponse,
};
use subspace_runtime::{RuntimeApi, RuntimeEvent as Event};
use subspace_runtime_primitives::opaque::{Block as RuntimeBlock, Header};
use subspace_service::segment_headers::SegmentHeaderCache;
use subspace_service::SubspaceConfiguration;

use crate::PosTable;

mod builder;
pub mod chain_spec;
#[cfg(feature = "executor")]
pub mod domains;

pub use builder::*;
#[cfg(feature = "executor")]
pub use domains::{ConfigBuilder as SystemDomainBuilder, SystemDomainNode};
pub use sdk_dsn::builder::*;
pub use sdk_substrate::*;

/// Trait which abstracts farmer for node
#[async_trait::async_trait]
pub trait Farmer {
    /// Fetch piece by its hash
    async fn get_piece_by_hash(
        piece_index_hash: subspace_core_primitives::PieceIndexHash,
        piece_store: &sdk_dsn::builder::PieceStore,
        weak_readers_and_pieces: &std::sync::Weak<
            parking_lot::Mutex<
                Option<subspace_farmer::utils::readers_and_pieces::ReadersAndPieces>,
            >,
        >,
        piece_memory_cache: &subspace_farmer_components::piece_caching::PieceMemoryCache,
    ) -> Option<subspace_core_primitives::Piece>;
}

impl<F: Farmer + 'static> Config<F> {
    /// Start a node with supplied parameters
    pub async fn build(
        self,
        directory: impl AsRef<Path>,
        chain_spec: ChainSpec,
    ) -> anyhow::Result<Node<F>> {
        let Self {
            base,
            piece_cache_size,
            mut dsn,
            #[cfg(feature = "executor")]
            system_domain,
            segment_publish_concurrency: SegmentPublishConcurrency(segment_publish_concurrency),
            sync_from_dsn,
            storage_monitor,
            ..
        } = self;
        let base = base.configuration(directory.as_ref(), chain_spec.clone()).await;
        let name = base.network.node_name.clone();
        let database_source = base.database.clone();

        let partial_components =
            subspace_service::new_partial::<PosTable, RuntimeApi, ExecutorDispatch>(&base)
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
            segment_publish_concurrency,
            subspace_networking,
            sync_from_dsn,
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
        let full_client = subspace_service::new_full::<PosTable, _, _, _>(
            configuration,
            partial_components,
            true,
            slot_proportion,
        )
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

        #[cfg(feature = "executor")]
        let (system_domain, full_client) = if let Some(config) = system_domain {
            use sc_service::ChainSpecExtension;
            use tracing_futures::Instrument;

            let span = tracing::info_span!("SystemDomain");
            let mut full_client = full_client;

            let system_domain_spec = chain_spec
                .extensions()
                .get_any(std::any::TypeId::of::<domains::ChainSpec>())
                .downcast_ref()
                .cloned()
                .ok_or_else(|| {
                    anyhow::anyhow!("Primary chain spec must contain system domain chain spec")
                })?;

            let node = SystemDomainNode::new(
                config,
                directory.as_ref().join("domains"),
                system_domain_spec,
                &mut full_client,
            )
            .instrument(span)
            .await
            .map(Some)?;

            (node, full_client)
        } else {
            (None, full_client)
        };

        let NewFull {
            mut task_manager,
            client,
            rpc_handlers,
            network_starter,
            sync_service,
            network_service,

            backend: _,
            select_chain: _,
            reward_signing_notification_stream: _,
            archived_segment_notification_stream: _,
            transaction_pool: _,
            block_importing_notification_stream: _,
            new_slot_notification_stream: _,
        } = full_client;

        let rpc_handle = sdk_utils::Rpc::new(&rpc_handlers);
        network_starter.start_network();
        let (stop_sender, mut stop_receiver) = mpsc::channel::<oneshot::Sender<()>>(1);

        sdk_utils::task_spawn(format!("subspace-sdk-node-{name}-task-manager"), {
            let dsn_informer = subspace_networking::utils::online_status_informer(&dsn.node);
            async move {
                let opt_stop_sender = async move {
                    futures::select! {
                        opt_sender = stop_receiver.next() => opt_sender,
                        result = task_manager.future().fuse() => {
                            result.expect("Task from manager paniced");
                            None
                        }
                        _ = node_runner_future.fuse() => {
                            tracing::info!("Node runner exited");
                            None
                        }
                        _ = dsn_informer.fuse() => {
                            tracing::info!("DSN online status observer exited");
                            None
                        }
                    }
                }
                .await;
                opt_stop_sender.map(|stop_sender| stop_sender.send(()));
            }
        });

        let drop_collection = DropCollection::new();

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
            #[cfg(feature = "executor")]
            system_domain,
            network_service,
            sync_service,
            name,
            rpc_handle,
            dsn,
            stop_sender,
            _drop_at_exit: drop_collection,
            _farmer: Default::default(),
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
pub struct Node<F: Farmer> {
    #[cfg(feature = "executor")]
    system_domain: Option<SystemDomainNode>,
    #[derivative(Debug = "ignore")]
    client: Arc<FullClient>,
    #[derivative(Debug = "ignore")]
    sync_service: Arc<sc_network_sync::service::chain_sync::SyncingService<RuntimeBlock>>,
    #[derivative(Debug = "ignore")]
    network_service: Arc<NetworkService<RuntimeBlock, Hash>>,

    pub(crate) rpc_handle: sdk_utils::Rpc,
    pub(crate) stop_sender: mpsc::Sender<oneshot::Sender<()>>,
    pub(crate) name: String,
    pub(crate) dsn: DsnShared<FullClient>,
    #[derivative(Debug = "ignore")]
    _drop_at_exit: DropCollection,
    #[derivative(Debug = "ignore")]
    _farmer: std::marker::PhantomData<F>,
}

static_assertions::assert_impl_all!(Node<crate::farmer::Farmer>: Send, Sync);

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
    pub pre_digest: Option<PreDigest<crate::PublicKey, crate::PublicKey>>,
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

    /// Gemini 3d configuration
    pub fn gemini_3d() -> Builder<F> {
        Builder::gemini_3d()
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
    #[cfg(feature = "executor")]
    pub fn system_domain(&self) -> Option<SystemDomainNode> {
        self.system_domain.as_ref().cloned()
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

fn get_piece_by_hash<F: Farmer>(
    &PieceByHashRequest { piece_index_hash }: &PieceByHashRequest,
    weak_readers_and_pieces: std::sync::Weak<
        parking_lot::Mutex<Option<subspace_farmer::utils::readers_and_pieces::ReadersAndPieces>>,
    >,
    farmer_piece_store: Arc<tokio::sync::Mutex<Option<sdk_dsn::builder::PieceStore>>>,
    piece_cache: NodePieceCache<impl sc_client_api::AuxStore>,
    piece_memory_cache: subspace_farmer_components::piece_caching::PieceMemoryCache,
) -> impl std::future::Future<Output = Option<PieceByHashResponse>> {
    async move {
        match node_get_piece_by_hash(piece_index_hash, &piece_cache) {
            Some(PieceByHashResponse { piece: None }) | None => (),
            result => return result,
        }

        if let Some(piece_store) = farmer_piece_store.lock().await.as_ref() {
            let piece = F::get_piece_by_hash(
                piece_index_hash,
                piece_store,
                &weak_readers_and_pieces,
                &piece_memory_cache,
            )
            .await;
            Some(PieceByHashResponse { piece })
        } else {
            None
        }
    }
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

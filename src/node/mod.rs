use anyhow::Context;
use futures::channel::{mpsc, oneshot};
use futures::{FutureExt, SinkExt, Stream, StreamExt};
use libp2p_core::Multiaddr;
use sc_client_api::client::BlockImportNotification;
use sc_executor::{WasmExecutionMethod, WasmtimeInstantiationStrategy};
use sc_network::config::{NodeKeyConfig, Secret};
use sc_network::network_state::NetworkState;
use sc_network::{NetworkService, NetworkStateInfo, NetworkStatusProvider, SyncState};
use sc_network_common::config::MultiaddrWithPeerId;
use sc_service::config::{KeystoreConfig, NetworkConfiguration, OffchainWorkerConfig};
use sc_service::{BasePath, Configuration, DatabaseSource, TracingReceiver};
use sc_subspace_chain_specs::ConsensusChainSpec;
use serde::{Deserialize, Serialize};
use sp_consensus::SyncOracle;
use sp_core::H256;
use std::io;
use std::num::NonZeroU64;
use std::path::Path;
use std::sync::{Arc, Weak};
use subspace_core_primitives::SolutionRange;
use subspace_farmer::RpcClient;
use subspace_farmer_components::FarmerProtocolInfo;
use subspace_rpc_primitives::SlotInfo;
use subspace_runtime::{GenesisConfig as ConsensusGenesisConfig, RuntimeApi};
use subspace_runtime_primitives::opaque::{Block as RuntimeBlock, Header};
use subspace_service::{FullClient, SubspaceConfiguration};
use system_domain_runtime::GenesisConfig as ExecutionGenesisConfig;

pub use sc_service::{
    config::{ExecutionStrategies, ExecutionStrategy},
    BlocksPruning, PruningMode,
};
pub use sc_state_db::Constraints;

pub mod chain_spec;

/// Role of the local node.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub enum Role {
    #[default]
    /// Regular full node.
    Full,
    /// Actual authority.
    Authority,
}

impl From<Role> for sc_service::Role {
    fn from(value: Role) -> Self {
        match value {
            Role::Full => sc_service::Role::Full,
            Role::Authority => sc_service::Role::Authority,
        }
    }
}

struct BlocksPruningInner(BlocksPruning);

impl Default for BlocksPruningInner {
    fn default() -> Self {
        Self(BlocksPruning::KeepAll)
    }
}

/// Available RPC methods.
#[derive(Debug, Copy, Clone, Default, Serialize, Deserialize)]
pub enum RpcMethods {
    /// Expose every RPC method only when RPC is listening on `localhost`,
    /// otherwise serve only safe RPC methods.
    #[default]
    Auto,
    /// Allow only a safe subset of RPC methods.
    Safe,
    /// Expose every RPC method (even potentially unsafe ones).
    Unsafe,
}

impl From<RpcMethods> for sc_service::RpcMethods {
    fn from(value: RpcMethods) -> Self {
        match value {
            RpcMethods::Auto => Self::Auto,
            RpcMethods::Safe => Self::Safe,
            RpcMethods::Unsafe => Self::Unsafe,
        }
    }
}

/// Node builder
#[derive(Default)]
pub struct Builder {
    name: Option<String>,
    force_authoring: bool,
    force_synced: bool,
    role: Role,
    blocks_pruning: BlocksPruningInner,
    state_pruning: Option<PruningMode>,
    listen_on: Vec<Multiaddr>,
    boot_nodes: Vec<MultiaddrWithPeerId>,
    rpc_methods: RpcMethods,
    execution_strategies: ExecutionStrategies,
}

impl Builder {
    /// New builder
    pub fn new() -> Self {
        Self::default()
    }

    /// Name of the node in the telemetry
    pub fn name(mut self, name: impl AsRef<str>) -> Self {
        if !name.as_ref().is_empty() {
            self.name = Some(name.as_ref().to_owned());
        }
        self
    }

    /// Force authoring of the blocks
    pub fn force_authoring(mut self, force_authoring: bool) -> Self {
        self.force_authoring = force_authoring;
        self
    }

    /// Force node to think it is synced
    pub fn force_synced(mut self, force_synced: bool) -> Self {
        self.force_synced = force_synced;
        self
    }

    /// Role of the node in the consensus
    pub fn role(mut self, role: Role) -> Self {
        self.role = role;
        self
    }

    /// Set blocks pruning settings
    pub fn blocks_pruning(mut self, pruning: BlocksPruning) -> Self {
        self.blocks_pruning = BlocksPruningInner(pruning);
        self
    }

    /// Set state pruning settings
    pub fn state_pruning(mut self, pruning: Option<PruningMode>) -> Self {
        self.state_pruning = pruning;
        self
    }

    /// RPC methods settings
    pub fn rpc_methods(mut self, rpc_methods: RpcMethods) -> Self {
        self.rpc_methods = rpc_methods;
        self
    }

    /// Listen on some address
    pub fn listen_on(mut self, listen_on: Vec<Multiaddr>) -> Self {
        self.listen_on = listen_on;
        self
    }

    /// Add boot nodes apart from ones from chainspec
    pub fn boot_nodes(mut self, boot_nodes: Vec<MultiaddrWithPeerId>) -> Self {
        self.boot_nodes = boot_nodes;
        self
    }

    /// Set block execution strategies for the node
    pub fn execution_strategies(mut self, execution_strategies: ExecutionStrategies) -> Self {
        self.execution_strategies = execution_strategies;
        self
    }

    /// Start a node with supplied parameters
    pub async fn build(
        self,
        directory: impl AsRef<Path>,
        chain_spec: ConsensusChainSpec<ConsensusGenesisConfig, ExecutionGenesisConfig>,
    ) -> anyhow::Result<Node> {
        const NODE_KEY_ED25519_FILE: &str = "secret_ed25519";
        const DEFAULT_NETWORK_CONFIG_PATH: &str = "network";

        let Self {
            name,
            force_authoring,
            force_synced,
            role,
            blocks_pruning: BlocksPruningInner(blocks_pruning),
            state_pruning,
            listen_on,
            boot_nodes,
            rpc_methods,
            execution_strategies,
        } = self;

        let base_path = BasePath::new(directory.as_ref());
        let impl_name = env!("CARGO_PKG_NAME").to_owned();
        let impl_version = format!("{}-{}", env!("CARGO_PKG_VERSION"), env!("GIT_HASH"));
        let config_dir = base_path.config_dir(chain_spec.id());
        let net_config_dir = config_dir.join(DEFAULT_NETWORK_CONFIG_PATH);
        let client_id = format!("{}/v{}", impl_name, impl_version);
        let mut network = NetworkConfiguration {
            listen_addresses: listen_on,
            boot_nodes: chain_spec
                .boot_nodes()
                .iter()
                .cloned()
                .chain(boot_nodes)
                .collect(),
            force_synced,
            ..NetworkConfiguration::new(
                name.clone().unwrap_or_default(),
                client_id,
                NodeKeyConfig::Ed25519(Secret::File(net_config_dir.join(NODE_KEY_ED25519_FILE))),
                Some(net_config_dir),
            )
        };

        // Increase default value of 25 to improve success rate of sync
        network.default_peers_set.out_peers = 50;
        // Full + Light clients
        network.default_peers_set.in_peers = 25 + 100;
        let (keystore_remote, keystore) = (None, KeystoreConfig::InMemory);
        let telemetry_endpoints = chain_spec.telemetry_endpoints().clone();

        // Default value are used for many of parameters
        let configuration = SubspaceConfiguration {
            base: Configuration {
                impl_name,
                impl_version,
                tokio_handle: tokio::runtime::Handle::current(),
                transaction_pool: Default::default(),
                network,
                keystore_remote,
                keystore,
                database: DatabaseSource::ParityDb {
                    path: config_dir.join("paritydb").join("full"),
                },
                trie_cache_maximum_size: Some(67_108_864),
                state_pruning,
                blocks_pruning,
                wasm_method: WasmExecutionMethod::Compiled {
                    instantiation_strategy: WasmtimeInstantiationStrategy::PoolingCopyOnWrite,
                },
                wasm_runtime_overrides: None,
                execution_strategies,
                rpc_http: None,
                rpc_ws: Some("127.0.0.1:9947".parse().expect("IP and port are valid")),
                rpc_ipc: None,
                // necessary in order to use `peers` method to show number of node peers during sync
                rpc_methods: rpc_methods.into(),
                rpc_ws_max_connections: Default::default(),
                // Below CORS are default from Substrate
                rpc_cors: Some(vec![
                    "http://localhost:*".to_string(),
                    "http://127.0.0.1:*".to_string(),
                    "https://localhost:*".to_string(),
                    "https://127.0.0.1:*".to_string(),
                    "https://polkadot.js.org".to_string(),
                    "http://localhost:3009".to_string(),
                ]),
                rpc_max_payload: None,
                rpc_max_request_size: None,
                rpc_max_response_size: None,
                rpc_id_provider: None,
                ws_max_out_buffer_capacity: None,
                prometheus_config: None,
                telemetry_endpoints,
                default_heap_pages: None,
                offchain_worker: OffchainWorkerConfig::default(),
                force_authoring,
                disable_grandpa: false,
                dev_key_seed: None,
                tracing_targets: None,
                tracing_receiver: TracingReceiver::Log,
                chain_spec: Box::new(chain_spec),
                max_runtime_instances: 8,
                announce_block: true,
                role: role.into(),
                base_path: Some(base_path),
                informant_output_format: Default::default(),
                runtime_cache_size: 2,
                rpc_max_subs_per_conn: None,
            },
            force_new_slot_notifications: false,
            dsn_config: None,
        };

        let slot_proportion = sc_consensus_slots::SlotProportion::new(2f32 / 3f32);
        let full_client = subspace_service::new_full::<RuntimeApi, ExecutorDispatch>(
            configuration,
            true,
            slot_proportion,
        )
        .await
        .context("Failed to build a full subspace node")?;

        let subspace_service::NewFull {
            mut task_manager,
            client,
            rpc_handlers,
            network_starter,
            network,

            select_chain: _,
            backend: _,
            new_slot_notification_stream: _,
            reward_signing_notification_stream: _,
            imported_block_notification_stream: _,
            archived_segment_notification_stream: _,
            transaction_pool: _,
        } = full_client;

        let client = Arc::downgrade(&client);
        let rpc_handle = rpc_handlers.handle();
        network_starter.start_network();
        let (stop_sender, mut stop_receiver) = mpsc::channel::<oneshot::Sender<()>>(1);

        tokio::spawn(async move {
            let stop_sender = futures::select! {
                opt_sender = stop_receiver.next() => {
                    match opt_sender {
                        Some(sender) => sender,
                        None => return,
                    }
                }
                result = task_manager.future().fuse() => {
                    let _ = result;
                    return;
                }
            };
            drop(task_manager);
            let _ = stop_sender.send(());
        });

        Ok(Node {
            client,
            network,
            name,
            rpc_handle,
            stop_sender,
        })
    }
}

/// Executor dispatch for subspace runtime
struct ExecutorDispatch;

impl sc_executor::NativeExecutionDispatch for ExecutorDispatch {
    // /// Only enable the benchmarking host functions when we actually want to benchmark.
    // #[cfg(feature = "runtime-benchmarks")]
    // type ExtendHostFunctions = frame_benchmarking::benchmarking::HostFunctions;
    // /// Otherwise we only use the default Substrate host functions.
    // #[cfg(not(feature = "runtime-benchmarks"))]
    type ExtendHostFunctions = ();

    fn dispatch(method: &str, data: &[u8]) -> Option<Vec<u8>> {
        subspace_runtime::api::dispatch(method, data)
    }

    fn native_version() -> sc_executor::NativeVersion {
        subspace_runtime::native_version()
    }
}

/// Node structure
#[derive(Clone)]
pub struct Node {
    client: Weak<FullClient<RuntimeApi, ExecutorDispatch>>,
    network: Arc<NetworkService<RuntimeBlock, Hash>>,
    rpc_handle: Arc<jsonrpsee_core::server::rpc_module::RpcModule<()>>,
    stop_sender: mpsc::Sender<oneshot::Sender<()>>,
    name: Option<String>,
}

impl std::fmt::Debug for Node {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Node")
            .field("rpc_handle", &self.rpc_handle)
            .field("stop_sender", &self.stop_sender)
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

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
    pub name: Option<String>,
    /// Number of peers connected to our node
    pub connected_peers: u64,
    /// Number of nodes that we know of but that we're not connected to
    pub not_connected_peers: u64,
    /// Total number of pieces stored on chain
    pub total_pieces: NonZeroU64,
    /// Range for solution
    pub solution_range: SolutionRange,
    /// Range for voting solutions
    pub voting_solution_range: SolutionRange,
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
    /// Is it new best block?
    pub is_new_best: bool,
}

impl Node {
    /// New node builder
    pub fn builder() -> Builder {
        Builder::new()
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

    /// Subscribe for node syncing progress
    pub async fn subscribe_syncing_progress(
        &self,
    ) -> impl Stream<Item = SyncState<BlockNumber>> + Send + Unpin + 'static {
        const CHECK_SYNCED_EVERY: std::time::Duration = std::time::Duration::from_millis(100);

        while self.network.is_offline() {
            tokio::time::sleep(CHECK_SYNCED_EVERY).await;
        }

        let network = Arc::clone(&self.network);
        let stream =
            tokio_stream::wrappers::IntervalStream::new(tokio::time::interval(CHECK_SYNCED_EVERY))
                .then(move |_| {
                    let network = Arc::clone(&network);
                    async move { network.status().await.map(|s| s.sync_state) }
                })
                .take_while(|status| {
                    futures::future::ready(!matches!(status, Ok(SyncState::Idle) | Err(())))
                })
                .map(|result_status| result_status.unwrap_or(SyncState::Idle));

        Box::pin(stream)
    }

    /// Wait till the end of node syncing
    pub async fn sync(&self) {
        self.subscribe_syncing_progress()
            .await
            .for_each(|_| async move {})
            .await
    }

    /// Leaves the network and gracefully shuts down
    pub async fn close(mut self) {
        let (stop_sender, stop_receiver) = oneshot::channel();
        drop(self.stop_sender.send(stop_sender).await);
        let _ = stop_receiver.await;
    }

    /// Runs `.close()` and also wipes node's state
    pub async fn wipe(path: impl AsRef<Path>) -> io::Result<()> {
        tokio::fs::remove_dir_all(path).await
    }

    fn client(&self) -> anyhow::Result<Arc<FullClient<RuntimeApi, ExecutorDispatch>>> {
        self.client
            .upgrade()
            .ok_or_else(|| anyhow::anyhow!("The node was already closed"))
    }

    /// Get node info
    pub async fn get_info(&self) -> anyhow::Result<Info> {
        let SlotInfo {
            solution_range,
            voting_solution_range,
            ..
        } = self
            .subscribe_slot_info()
            .await
            .map_err(anyhow::Error::msg)?
            .next()
            .await
            .expect("This stream never ends");
        let version = self
            .rpc_handle
            .call("state_getRuntimeVersion", &[] as &[()])
            .await?;
        let client = self.client().context("Failed to fetch node info")?;
        let NetworkState {
            connected_peers,
            not_connected_peers,
            ..
        } = self.network.network_state().await.unwrap();
        let sp_blockchain::Info {
            best_hash,
            best_number,
            genesis_hash,
            finalized_hash,
            finalized_number,
            block_gap,
            ..
        } = client.chain_info();
        let FarmerProtocolInfo { total_pieces, .. } = self
            .farmer_protocol_info()
            .await
            .map_err(anyhow::Error::msg)?;
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
            solution_range,
            voting_solution_range,
        })
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
                     header:
                         Header {
                             parent_hash,
                             number,
                             state_root,
                             extrinsics_root,
                             digest: _,
                         },
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

fn subscription_to_stream<T: serde::de::DeserializeOwned>(
    mut subscription: jsonrpsee_core::server::rpc_module::Subscription,
) -> impl Stream<Item = T> + Unpin {
    futures::stream::poll_fn(move |cx| {
        Box::pin(subscription.next())
            .poll_unpin(cx)
            .map(|x| x.and_then(Result::ok).map(|(x, _)| x))
    })
}

mod farmer_rpc_client {
    use super::*;

    use futures::Stream;
    use std::pin::Pin;

    use subspace_archiving::archiver::ArchivedSegment;
    use subspace_core_primitives::{Piece, PieceIndex, RecordsRoot, SegmentIndex};
    use subspace_farmer::rpc_client::{Error, RpcClient};
    use subspace_farmer_components::FarmerProtocolInfo;
    use subspace_rpc_primitives::{
        RewardSignatureResponse, RewardSigningInfo, SlotInfo, SolutionResponse,
    };

    #[async_trait::async_trait]
    impl RpcClient for Node {
        async fn farmer_protocol_info(&self) -> Result<FarmerProtocolInfo, Error> {
            Ok(self
                .rpc_handle
                .call("subspace_getFarmerProtocolInfo", &[] as &[()])
                .await?)
        }

        async fn subscribe_slot_info(
            &self,
        ) -> Result<Pin<Box<dyn Stream<Item = SlotInfo> + Send + 'static>>, Error> {
            Ok(Box::pin(subscription_to_stream(
                self.rpc_handle
                    .subscribe("subspace_subscribeSlotInfo", &[] as &[()])
                    .await?,
            )))
        }

        async fn submit_solution_response(
            &self,
            solution_response: SolutionResponse,
        ) -> Result<(), Error> {
            Ok(self
                .rpc_handle
                .call("subspace_submitSolutionResponse", [solution_response])
                .await?)
        }

        async fn subscribe_reward_signing(
            &self,
        ) -> Result<Pin<Box<dyn Stream<Item = RewardSigningInfo> + Send + 'static>>, Error>
        {
            Ok(Box::pin(subscription_to_stream(
                self.rpc_handle
                    .subscribe("subspace_subscribeRewardSigning", &[] as &[()])
                    .await?,
            )))
        }

        async fn submit_reward_signature(
            &self,
            reward_signature: RewardSignatureResponse,
        ) -> Result<(), Error> {
            Ok(self
                .rpc_handle
                .call("subspace_submitRewardSignature", [reward_signature])
                .await?)
        }

        async fn subscribe_archived_segments(
            &self,
        ) -> Result<Pin<Box<dyn Stream<Item = ArchivedSegment> + Send + 'static>>, Error> {
            Ok(Box::pin(subscription_to_stream(
                self.rpc_handle
                    .subscribe("subspace_subscribeArchivedSegment", &[] as &[()])
                    .await?,
            )))
        }

        async fn records_roots(
            &self,
            segment_indexes: Vec<SegmentIndex>,
        ) -> Result<Vec<Option<RecordsRoot>>, Error> {
            Ok(self
                .rpc_handle
                .call("subspace_recordsRoots", [segment_indexes])
                .await?)
        }

        async fn get_piece(&self, piece_index: PieceIndex) -> Result<Option<Piece>, Error> {
            Ok(self
                .rpc_handle
                .call("subspace_getPiece", [piece_index])
                .await?)
        }
    }
}

#[cfg(test)]
mod tests {
    use subspace_farmer::RpcClient;
    use tempdir::TempDir;

    use crate::{Farmer, PlotDescription};

    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn test_start_node() {
        let dir = TempDir::new("test").unwrap();
        Node::builder()
            .build(dir.path(), chain_spec::dev_config().unwrap())
            .await
            .unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_rpc() {
        let dir = TempDir::new("test").unwrap();
        let node = Node::builder()
            .build(dir.path(), chain_spec::dev_config().unwrap())
            .await
            .unwrap();

        assert!(node.farmer_protocol_info().await.is_ok());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_closing() {
        let dir = TempDir::new("test").unwrap();
        let node = Node::builder()
            .force_authoring(true)
            .role(Role::Authority)
            .build(dir.path(), chain_spec::dev_config().unwrap())
            .await
            .unwrap();
        let plot_dir = TempDir::new("test").unwrap();
        let plots = [PlotDescription::new(
            plot_dir.as_ref(),
            bytesize::ByteSize::mib(32),
        )];
        let farmer = Farmer::builder()
            .build(Default::default(), node.clone(), &plots)
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;

        farmer.close().await;
        node.close().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "Works most of times though"]
    async fn test_sync_block() {
        let dir = TempDir::new("test").unwrap();
        let chain = chain_spec::dev_config().unwrap();
        let node = Node::builder()
            .force_authoring(true)
            .force_synced(true)
            .role(Role::Authority)
            .listen_on(vec!["/ip4/127.0.0.1/tcp/0".parse().unwrap()])
            .build(dir.path(), chain.clone())
            .await
            .unwrap();
        let plot_dir = TempDir::new("test").unwrap();
        let farmer = Farmer::builder()
            .build(
                Default::default(),
                node.clone(),
                &[PlotDescription::new(
                    plot_dir.as_ref(),
                    bytesize::ByteSize::gb(1),
                )],
            )
            .await
            .unwrap();

        let farm_blocks = 4;

        node.subscribe_new_blocks()
            .await
            .unwrap()
            .skip_while(|notification| futures::future::ready(notification.number < farm_blocks))
            .next()
            .await
            .unwrap();

        farmer.close().await;

        let dir = TempDir::new("test").unwrap();
        let other_node = Node::builder()
            .force_authoring(true)
            .role(Role::Authority)
            .boot_nodes(node.listen_addresses().await.unwrap())
            .build(dir.path(), chain)
            .await
            .unwrap();

        other_node
            .subscribe_syncing_progress()
            .await
            .for_each(|_| async {})
            .await;
        assert_eq!(
            other_node.get_info().await.unwrap().best_block.1,
            farm_blocks
        );

        node.close().await;
        other_node.close().await;
    }
}

use std::io;
use std::num::NonZeroU64;
use std::path::Path;
use std::sync::{Arc, Weak};
use std::time::Duration;

use anyhow::Context;
use futures::channel::{mpsc, oneshot};
use futures::{FutureExt, SinkExt, Stream, StreamExt};
use libp2p_core::Multiaddr;
use sc_client_api::client::BlockImportNotification;
use sc_executor::{WasmExecutionMethod, WasmtimeInstantiationStrategy};
use sc_network::config::{NodeKeyConfig, Secret};
use sc_network::network_state::NetworkState;
use sc_network::{NetworkService, NetworkStateInfo, NetworkStatusProvider, SyncState};
use sc_network_common::config::{MultiaddrWithPeerId, TransportConfig};
use sc_service::config::{KeystoreConfig, NetworkConfiguration, OffchainWorkerConfig};
use sc_service::{BasePath, Configuration, DatabaseSource, TracingReceiver};
use sc_subspace_chain_specs::ConsensusChainSpec;
use serde::{Deserialize, Serialize};
use sp_consensus::SyncOracle;
use sp_core::H256;
use subspace_core_primitives::SolutionRange;
use subspace_farmer::RpcClient;
use subspace_farmer_components::FarmerProtocolInfo;
use subspace_networking::{PieceByHashRequest, PieceByHashRequestHandler};
use subspace_rpc_primitives::SlotInfo;
use subspace_runtime::{GenesisConfig as ConsensusGenesisConfig, RuntimeApi};
use subspace_runtime_primitives::opaque::{Block as RuntimeBlock, Header};
use subspace_service::{FullClient, SubspaceConfiguration};
use system_domain_runtime::GenesisConfig as ExecutionGenesisConfig;

pub mod chain_spec;

pub use builder::{
    BlocksPruning, Builder, Config, Constraints, Dsn, DsnBuilder, ExecutionStrategy, Network,
    NetworkBuilder, OffchainWorker, OffchainWorkerBuilder, PruningMode, Rpc, RpcBuilder,
};

use crate::networking::{
    FarmerRecordStorage, MaybeRecordStorage, NodeRecordStorage, ReadersAndPieces, RecordStorage,
};

mod builder {
    use std::net::SocketAddr;
    use std::num::NonZeroUsize;

    use derivative::Derivative;
    use derive_builder::Builder;
    use serde::{Deserialize, Serialize};

    use super::*;

    /// Block pruning settings.
    #[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
    pub enum BlocksPruning {
        #[default]
        /// Keep full block history, of every block that was ever imported.
        KeepAll,
        /// Keep full finalized block history.
        KeepFinalized,
        /// Keep N recent finalized blocks.
        Some(u32),
    }

    impl From<sc_service::BlocksPruning> for BlocksPruning {
        fn from(value: sc_service::BlocksPruning) -> Self {
            match value {
                sc_service::BlocksPruning::KeepAll => Self::KeepAll,
                sc_service::BlocksPruning::KeepFinalized => Self::KeepFinalized,
                sc_service::BlocksPruning::Some(n) => Self::Some(n),
            }
        }
    }

    impl From<BlocksPruning> for sc_service::BlocksPruning {
        fn from(value: BlocksPruning) -> Self {
            match value {
                BlocksPruning::KeepAll => Self::KeepAll,
                BlocksPruning::KeepFinalized => Self::KeepFinalized,
                BlocksPruning::Some(n) => Self::Some(n),
            }
        }
    }

    /// Pruning constraints. If none are specified pruning is
    #[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
    pub struct Constraints {
        /// Maximum blocks. Defaults to 0 when unspecified, effectively keeping
        /// only non-canonical states.
        pub max_blocks: Option<u32>,
        /// Maximum memory in the pruning overlay.
        pub max_mem: Option<usize>,
    }

    impl From<Constraints> for sc_state_db::Constraints {
        fn from(Constraints { max_blocks, max_mem }: Constraints) -> Self {
            Self { max_blocks, max_mem }
        }
    }

    impl From<sc_state_db::Constraints> for Constraints {
        fn from(
            sc_state_db::Constraints { max_blocks, max_mem }: sc_state_db::Constraints,
        ) -> Self {
            Self { max_blocks, max_mem }
        }
    }

    /// Pruning mode.
    #[derive(Debug, Clone, Eq, PartialEq, Default, Serialize, Deserialize)]
    pub enum PruningMode {
        /// No pruning. Canonicalization is a no-op.
        #[default]
        ArchiveAll,
        /// Canonicalization discards non-canonical nodes. All the canonical
        /// nodes are kept in the DB.
        ArchiveCanonical,
        /// Maintain a pruning window.
        Constrained(Constraints),
    }

    impl From<PruningMode> for sc_service::PruningMode {
        fn from(value: PruningMode) -> Self {
            match value {
                PruningMode::ArchiveAll => Self::ArchiveAll,
                PruningMode::ArchiveCanonical => Self::ArchiveCanonical,
                PruningMode::Constrained(c) => Self::Constrained(c.into()),
            }
        }
    }

    impl From<sc_service::PruningMode> for PruningMode {
        fn from(value: sc_service::PruningMode) -> Self {
            match value {
                sc_service::PruningMode::ArchiveAll => Self::ArchiveAll,
                sc_service::PruningMode::ArchiveCanonical => Self::ArchiveCanonical,
                sc_service::PruningMode::Constrained(c) => Self::Constrained(c.into()),
            }
        }
    }

    /// Strategy for executing a call into the runtime.
    #[derive(Copy, Clone, Eq, PartialEq, Debug, Default, Deserialize, Serialize)]
    pub enum ExecutionStrategy {
        /// Execute with the native equivalent if it is compatible with the
        /// given wasm module; otherwise fall back to the wasm.
        #[default]
        NativeWhenPossible,
        /// Use the given wasm module.
        AlwaysWasm,
        /// Run with both the wasm and the native variant (if compatible).
        /// Report any discrepancy as an error.
        Both,
        /// First native, then if that fails or is not possible, wasm.
        NativeElseWasm,
    }

    impl From<sc_service::config::ExecutionStrategy> for ExecutionStrategy {
        fn from(value: sc_service::config::ExecutionStrategy) -> Self {
            use sc_service::config::ExecutionStrategy as Other;
            match value {
                Other::Both => Self::Both,
                Other::AlwaysWasm => Self::AlwaysWasm,
                Other::NativeWhenPossible => Self::NativeWhenPossible,
                Other::NativeElseWasm => Self::NativeElseWasm,
            }
        }
    }

    impl From<ExecutionStrategy> for sc_service::config::ExecutionStrategy {
        fn from(value: ExecutionStrategy) -> Self {
            match value {
                ExecutionStrategy::Both => Self::Both,
                ExecutionStrategy::AlwaysWasm => Self::AlwaysWasm,
                ExecutionStrategy::NativeWhenPossible => Self::NativeWhenPossible,
                ExecutionStrategy::NativeElseWasm => Self::NativeElseWasm,
            }
        }
    }

    impl From<ExecutionStrategy> for sc_service::config::ExecutionStrategies {
        fn from(value: ExecutionStrategy) -> Self {
            sc_service::config::ExecutionStrategies {
                syncing: value.into(),
                importing: value.into(),
                block_construction: value.into(),
                offchain_worker: value.into(),
                other: value.into(),
            }
        }
    }

    fn default_piece_cache_size() -> bytesize::ByteSize {
        bytesize::ByteSize::gib(1)
    }

    fn default_impl_name() -> String {
        env!("CARGO_PKG_NAME").to_owned()
    }

    fn default_impl_version() -> String {
        format!("{}-{}", env!("CARGO_PKG_VERSION"), env!("GIT_HASH"))
    }

    fn default_segment_publish_concurrency() -> NonZeroUsize {
        NonZeroUsize::new(10).unwrap()
    }

    /// Node builder
    #[derive(Debug, Clone, Derivative, Builder, Deserialize, Serialize)]
    #[derivative(Default)]
    #[builder(pattern = "immutable", build_fn(name = "_build"), name = "Builder")]
    #[non_exhaustive]
    pub struct Config {
        /// Force block authoring
        #[builder(default)]
        #[serde(default)]
        pub force_authoring: bool,
        /// Max number of segments that can be published concurrently, impacts
        /// RAM usage and network bandwidth.
        #[builder(default = "default_segment_publish_concurrency()")]
        #[derivative(Default(value = "default_segment_publish_concurrency()"))]
        #[serde(default = "default_segment_publish_concurrency")]
        pub segment_publish_concurrency: NonZeroUsize,
        /// Set node role
        #[builder(default)]
        #[serde(default)]
        pub role: Role,
        /// Blocks pruning options
        #[builder(default)]
        #[serde(default)]
        pub blocks_pruning: BlocksPruning,
        /// State pruning options
        #[builder(default)]
        #[serde(default)]
        pub state_pruning: PruningMode,
        /// Set execution strategies
        #[builder(default)]
        #[serde(default)]
        pub execution_strategy: ExecutionStrategy,
        /// Set piece cache size
        #[builder(default = "default_piece_cache_size()")]
        #[derivative(Default(value = "default_piece_cache_size()"))]
        #[serde(with = "bytesize_serde", default = "default_piece_cache_size")]
        pub piece_cache_size: bytesize::ByteSize,
        /// Implementation name
        #[builder(default = "default_impl_name()")]
        #[derivative(Default(value = "default_impl_name()"))]
        #[serde(default = "default_impl_name")]
        pub impl_name: String,
        /// Implementation version
        #[builder(default = "default_impl_version()")]
        #[derivative(Default(value = "default_impl_version()"))]
        #[serde(default = "default_impl_version")]
        pub impl_version: String,
        /// Rpc settings
        #[builder(setter(into), default)]
        #[serde(default)]
        pub rpc: Rpc,
        /// Network settings
        #[builder(setter(into), default)]
        #[serde(default)]
        pub network: Network,
        /// DSN settings
        #[builder(setter(into), default)]
        #[serde(default)]
        pub dsn: Dsn,
        /// Offchain worker settings
        #[builder(setter(into), default)]
        #[serde(default)]
        pub offchain_worker: OffchainWorker,
    }

    fn default_max_subs_per_conn() -> usize {
        1024
    }

    /// Node RPC builder
    #[derive(Debug, Clone, Derivative, Builder, Deserialize, Serialize)]
    #[derivative(Default)]
    #[builder(pattern = "owned", build_fn(name = "_build"), name = "RpcBuilder")]
    #[non_exhaustive]
    pub struct Rpc {
        /// RPC over HTTP binding address. `None` if disabled.
        #[builder(setter(strip_option), default)]
        #[serde(default)]
        pub http: Option<SocketAddr>,
        /// RPC over Websockets binding address. `None` if disabled.
        #[builder(setter(strip_option), default)]
        #[serde(default)]
        pub ws: Option<SocketAddr>,
        /// RPC over IPC binding path. `None` if disabled.
        #[builder(setter(strip_option), default)]
        #[serde(default)]
        pub ipc: Option<String>,
        /// Maximum number of connections for WebSockets RPC server. `None` if
        /// default.
        #[builder(setter(strip_option), default)]
        #[serde(default)]
        pub ws_max_connections: Option<usize>,
        /// CORS settings for HTTP & WS servers. `None` if all origins are
        /// allowed.
        #[builder(setter(strip_option), default)]
        #[serde(default)]
        pub cors: Option<Vec<String>>,
        /// RPC methods to expose (by default only a safe subset or all of
        /// them).
        #[builder(default)]
        #[serde(default)]
        pub methods: RpcMethods,
        /// Maximum payload of rpc request/responses.
        #[builder(setter(strip_option), default)]
        #[serde(default)]
        pub max_payload: Option<usize>,
        /// Maximum payload of a rpc request
        #[builder(setter(strip_option), default)]
        #[serde(default)]
        pub max_request_size: Option<usize>,
        /// Maximum payload of a rpc request
        #[builder(setter(strip_option), default)]
        #[serde(default)]
        pub max_response_size: Option<usize>,
        /// Maximum allowed subscriptions per rpc connection
        #[builder(default = "default_max_subs_per_conn()")]
        #[derivative(Default(value = "default_max_subs_per_conn()"))]
        #[serde(default = "default_max_subs_per_conn")]
        pub max_subs_per_conn: usize,
        /// Maximum size of the output buffer capacity for websocket
        /// connections.
        #[builder(setter(strip_option), default)]
        #[serde(default)]
        pub ws_max_out_buffer_capacity: Option<usize>,
    }

    /// Node network builder
    #[derive(Debug, Default, Clone, Builder, Deserialize, Serialize)]
    #[builder(pattern = "owned", build_fn(name = "_build"), name = "NetworkBuilder")]
    #[non_exhaustive]
    pub struct Network {
        /// Listen on some address for other nodes
        #[builder(default)]
        #[serde(default)]
        pub enable_mdns: bool,
        /// Listen on some address for other nodes
        #[builder(default)]
        #[serde(default)]
        pub allow_private_ipv4: bool,
        /// Listen on some address for other nodes
        #[builder(default)]
        #[serde(default)]
        pub listen_addresses: Vec<Multiaddr>,
        /// Boot nodes
        #[builder(default)]
        #[serde(default)]
        pub boot_nodes: Vec<MultiaddrWithPeerId>,
        /// Force node to think it is synced
        #[builder(default)]
        #[serde(default)]
        pub force_synced: bool,
        /// Node name
        #[builder(setter(into, strip_option), default)]
        #[serde(default)]
        pub name: Option<String>,
        /// Client id for telemetry (default is `{IMPL_NAME}/v{IMPL_VERSION}`)
        #[builder(setter(into, strip_option), default)]
        #[serde(default)]
        pub client_id: Option<String>,
    }

    fn default_listen_addresses() -> Vec<libp2p_core::Multiaddr> {
        // TODO: get rid of it, once it won't be required by monorepo
        vec!["/ip4/127.0.0.1/tcp/0".parse().expect("Always valid")]
    }

    fn default_piece_publisher_batch_size() -> usize {
        15
    }

    /// Node DSN builder
    #[derive(Debug, Clone, Derivative, Builder, Deserialize, Serialize)]
    #[derivative(Default)]
    #[builder(pattern = "owned", build_fn(name = "_build"), name = "DsnBuilder")]
    #[non_exhaustive]
    pub struct Dsn {
        /// Listen on some address for other nodes
        #[builder(default = "default_listen_addresses()")]
        #[derivative(Default(value = "default_listen_addresses()"))]
        #[serde(default = "default_listen_addresses")]
        pub listen_addresses: Vec<libp2p_core::Multiaddr>,
        /// Boot nodes
        #[builder(default)]
        #[serde(default)]
        pub boot_nodes: Vec<libp2p_core::Multiaddr>,
        /// Reserved nodes
        #[builder(default)]
        #[serde(default)]
        pub reserved_nodes: Vec<libp2p_core::Multiaddr>,
        /// Determines whether we allow keeping non-global (private, shared,
        /// loopback..) addresses in Kademlia DHT.
        #[builder(default)]
        #[serde(default)]
        pub allow_non_global_addresses_in_dht: bool,
        /// Sets piece publisher batch size
        #[builder(default = "default_piece_publisher_batch_size()")]
        #[derivative(Default(value = "default_piece_publisher_batch_size()"))]
        #[serde(default = "default_piece_publisher_batch_size")]
        pub piece_publisher_batch_size: usize,
    }

    /// Offchain worker config
    #[derive(Debug, Clone, Derivative, Builder, Deserialize, Serialize)]
    #[derivative(Default)]
    #[builder(pattern = "owned", build_fn(name = "_build"), name = "OffchainWorkerBuilder")]
    #[non_exhaustive]
    pub struct OffchainWorker {
        /// Is enabled
        #[builder(default)]
        #[serde(default)]
        pub enabled: bool,
        /// Is indexing enabled
        #[builder(default)]
        #[serde(default)]
        pub indexing_enabled: bool,
    }

    impl From<OffchainWorker> for OffchainWorkerConfig {
        fn from(OffchainWorker { enabled, indexing_enabled }: OffchainWorker) -> Self {
            Self { enabled, indexing_enabled }
        }
    }

    crate::generate_builder!(Rpc, Network, Dsn, OffchainWorker);
}

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

const NODE_NAME_MAX_LENGTH: usize = 64;

impl Builder {
    /// Get configuration for saving on disk
    pub fn configuration(&self) -> Config {
        self._build().expect("Build is infallible")
    }

    /// New builder
    pub fn new() -> Self {
        Self::default()
    }

    /// Start a node with supplied parameters
    pub async fn build(
        self,
        directory: impl AsRef<Path>,
        chain_spec: ConsensusChainSpec<ConsensusGenesisConfig, ExecutionGenesisConfig>,
    ) -> anyhow::Result<Node> {
        self.configuration().build(directory, chain_spec).await
    }
}

impl Config {
    /// Start a node with supplied parameters
    pub async fn build(
        self,
        directory: impl AsRef<Path>,
        chain_spec: ConsensusChainSpec<ConsensusGenesisConfig, ExecutionGenesisConfig>,
    ) -> anyhow::Result<Node> {
        const NODE_KEY_ED25519_FILE: &str = "secret_ed25519";
        const DEFAULT_NETWORK_CONFIG_PATH: &str = "network";

        let Self {
            force_authoring,
            role,
            segment_publish_concurrency,
            blocks_pruning,
            state_pruning,
            execution_strategy,
            piece_cache_size,
            impl_name,
            impl_version,
            rpc:
                builder::Rpc {
                    http: rpc_http,
                    ws: rpc_ws,
                    ws_max_connections: rpc_ws_max_connections,
                    ipc: rpc_ipc,
                    cors: rpc_cors,
                    methods: rpc_methods,
                    max_payload: rpc_max_payload,
                    max_request_size: rpc_max_request_size,
                    max_response_size: rpc_max_response_size,
                    max_subs_per_conn: rpc_max_subs_per_conn,
                    ws_max_out_buffer_capacity,
                },
            network,
            dsn,
            offchain_worker,
        } = self;
        let base_path = BasePath::new(directory.as_ref());
        let config_dir = base_path.config_dir(chain_spec.id());

        let (mut network, name) = {
            let builder::Network {
                listen_addresses,
                boot_nodes,
                force_synced,
                name,
                client_id,
                enable_mdns,
                allow_private_ipv4,
            } = network;
            let name = name.unwrap_or_else(|| {
                names::Generator::with_naming(names::Name::Numbered)
                    .next()
                    .filter(|name| name.chars().count() < NODE_NAME_MAX_LENGTH)
                    .expect("RNG is available on all supported platforms; qed")
            });

            let client_id = client_id.unwrap_or_else(|| format!("{}/v{}", impl_name, impl_version));
            let config_dir = config_dir.join(DEFAULT_NETWORK_CONFIG_PATH);

            (
                NetworkConfiguration {
                    listen_addresses,
                    boot_nodes: chain_spec.boot_nodes().iter().cloned().chain(boot_nodes).collect(),
                    force_synced,
                    transport: TransportConfig::Normal { enable_mdns, allow_private_ipv4 },
                    ..NetworkConfiguration::new(
                        name.clone(),
                        client_id,
                        NodeKeyConfig::Ed25519(Secret::File(
                            config_dir.join(NODE_KEY_ED25519_FILE),
                        )),
                        Some(config_dir),
                    )
                },
                name,
            )
        };

        // Increase default value of 25 to improve success rate of sync
        network.default_peers_set.out_peers = 50;
        // Full + Light clients
        network.default_peers_set.in_peers = 25 + 100;
        let (keystore_remote, keystore) = (None, KeystoreConfig::InMemory);
        let telemetry_endpoints = chain_spec.telemetry_endpoints().clone();

        let base = Configuration {
            impl_name,
            impl_version,
            tokio_handle: tokio::runtime::Handle::current(),
            transaction_pool: Default::default(),
            network,
            keystore_remote,
            keystore,
            database: DatabaseSource::ParityDb { path: config_dir.join("paritydb").join("full") },
            trie_cache_maximum_size: Some(67_108_864),
            state_pruning: Some(state_pruning.into()),
            blocks_pruning: blocks_pruning.into(),
            wasm_method: WasmExecutionMethod::Compiled {
                instantiation_strategy: WasmtimeInstantiationStrategy::PoolingCopyOnWrite,
            },
            wasm_runtime_overrides: None,
            execution_strategies: execution_strategy.into(),
            rpc_http,
            rpc_ws,
            rpc_ipc,
            rpc_methods: rpc_methods.into(),
            rpc_ws_max_connections,
            rpc_cors,
            rpc_max_payload,
            rpc_max_request_size,
            rpc_max_response_size,
            rpc_id_provider: None,
            rpc_max_subs_per_conn: Some(rpc_max_subs_per_conn),
            ws_max_out_buffer_capacity,
            prometheus_config: None,
            telemetry_endpoints,
            default_heap_pages: None,
            offchain_worker: offchain_worker.into(),
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
        };

        let partial_components =
            subspace_service::new_partial::<RuntimeApi, ExecutorDispatch>(&base)
                .context("Failed to build a partial subspace node")?;
        let readers_and_pieces = Arc::new(std::sync::Mutex::new(None::<ReadersAndPieces>));
        let farmer_record_storage = MaybeRecordStorage::none();

        let (subspace_networking, (node, mut node_runner)) = {
            let builder::Dsn {
                listen_addresses,
                boot_nodes,
                reserved_nodes: _,
                allow_non_global_addresses_in_dht,
                piece_publisher_batch_size,
            } = dsn;
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
            let bootstrap_nodes = base
                .chain_spec
                .properties()
                .get("dsnBootstrapNodes")
                .cloned()
                .map(serde_json::from_value::<Vec<_>>)
                .transpose()
                .context("Failed to decode DSN bootsrap nodes")?
                .unwrap_or_default()
                .into_iter()
                .chain(boot_nodes)
                .map(|a| {
                    a.to_string()
                        .parse()
                        .expect("Convertion between 2 libp2p version. Never panics")
                })
                .collect::<Vec<_>>();

            let listen_on = listen_addresses
                .into_iter()
                .map(|a| {
                    a.to_string()
                        .parse()
                        .expect("Convertion between 2 libp2p version. Never panics")
                })
                .collect();

            let piece_cache = NodeRecordStorage::new(
                partial_components.client.clone(),
                piece_cache_size.as_u64(),
            );

            // Start before archiver below, so we don't have potential race condition and
            // miss pieces
            tokio::spawn({
                let piece_cache = piece_cache.clone();
                let mut archived_segment_notification_stream =
                    partial_components.other.1.archived_segment_notification_stream().subscribe();

                async move {
                    while let Some(archived_segment_notification) =
                        archived_segment_notification_stream.next().await
                    {
                        let segment_index = archived_segment_notification
                            .archived_segment
                            .root_block
                            .segment_index();
                        if let Err(error) = piece_cache.add_pieces(
                            segment_index * u64::from(subspace_core_primitives::PIECES_IN_SEGMENT),
                            &archived_segment_notification.archived_segment.pieces,
                        ) {
                            tracing::error!(%segment_index, %error, "Failed to store pieces for segment in cache");
                        }
                    }
                }
            });

            let record_store = subspace_networking::CustomRecordStore::new(
                RecordStorage::new(farmer_record_storage.clone(), piece_cache.clone()),
                subspace_networking::MemoryProviderStorage::default(),
            );
            let readers_and_pieces = Arc::downgrade(&readers_and_pieces);

            let config = subspace_networking::Config {
                keypair,
                listen_on,
                allow_non_global_addresses_in_dht,
                networking_parameters_registry:
                    subspace_networking::BootstrappedNetworkingParameters::new(
                        bootstrap_nodes.clone(),
                    )
                    .boxed(),
                request_response_protocols: vec![PieceByHashRequestHandler::create(
                    move |PieceByHashRequest { key }| {
                        let piece = match key {
                            subspace_networking::PieceKey::PieceIndex(piece_index) =>
                                match piece_cache.get_piece(*piece_index) {
                                    Ok(maybe_piece) => maybe_piece,
                                    Err(error) => {
                                        tracing::error!(?key, %error, "Failed to get piece from cache");
                                        None
                                    }
                                },
                            subspace_networking::PieceKey::Sector(piece_index_hash) => {
                                let Some(readers_and_pieces) = readers_and_pieces.upgrade() else {
                                    tracing::debug!("A readers and pieces are already dropped");
                                    return None
                                };
                                let readers_and_pieces = readers_and_pieces
                                    .lock()
                                    .expect("Readers lock is never poisoned");
                                let Some(readers_and_pieces) = readers_and_pieces.as_ref() else {
                                    tracing::debug!(?piece_index_hash, "Readers and pieces are not initialized yet");
                                    return None
                                };
                                readers_and_pieces.get_piece(piece_index_hash)
                            }
                            subspace_networking::PieceKey::PieceIndexHash(_piece_index_hash) => {
                                tracing::debug!(
                                    ?key,
                                    "Incorrect piece request - unsupported key type."
                                );
                                return None;
                            }
                        };

                        Some(subspace_networking::PieceByHashResponse { piece })
                    },
                )],
                record_store,
                ..subspace_networking::Config::with_generated_keypair()
            };

            let (node, node_runner) = subspace_networking::create(config).await?;

            (
                subspace_service::SubspaceNetworking::Reuse {
                    node: node.clone(),
                    bootstrap_nodes,
                    piece_publisher_batch_size,
                },
                (node, node_runner),
            )
        };

        // Default value are used for many of parameters
        let configuration = SubspaceConfiguration {
            base,
            force_new_slot_notifications: false,
            segment_publish_concurrency,
            subspace_networking,
        };

        tokio::spawn(async move {
            node_runner.run().await;
        });

        let slot_proportion = sc_consensus_slots::SlotProportion::new(2f32 / 3f32);
        let full_client =
            subspace_service::new_full(configuration, partial_components, true, slot_proportion)
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
                    result.expect("Task from manager paniced");
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
            farmer_record_storage,
            rpc_handle,
            readers_and_pieces,
            dsn_node: node,
            stop_sender,
        })
    }
}

/// Executor dispatch for subspace runtime
struct ExecutorDispatch;

impl sc_executor::NativeExecutionDispatch for ExecutorDispatch {
    // /// Only enable the benchmarking host functions when we actually want to
    // benchmark. #[cfg(feature = "runtime-benchmarks")]
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
    pub(crate) farmer_record_storage: MaybeRecordStorage<FarmerRecordStorage>,
    pub(crate) readers_and_pieces: Arc<std::sync::Mutex<Option<ReadersAndPieces>>>,
    pub(crate) dsn_node: subspace_networking::Node,
    name: String,
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
    pub name: String,
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

impl<S: Stream<Item = SyncingProgress>> Stream for SyncingProgressStream<S> {
    type Item = SyncingProgress;

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        let this = self.project();
        let next = this.inner.poll_next(cx);
        if let std::task::Poll::Ready(Some(SyncingProgress { at, target, .. })) = next {
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
    ) -> anyhow::Result<impl Stream<Item = SyncingProgress> + Send + Unpin + 'static> {
        const CHECK_SYNCED_EVERY: Duration = Duration::from_millis(100);
        const SYNCING_TIMEOUT: Duration = Duration::from_secs(60);

        let backoff = backoff::ExponentialBackoff {
            max_elapsed_time: Some(SYNCING_TIMEOUT),
            ..backoff::ExponentialBackoff::default()
        };

        backoff::retry(backoff.clone(), || {
            if self.network.is_offline() {
                Err(backoff::Error::transient(()))
            } else {
                Ok(())
            }
        })
        .map_err(|_| anyhow::anyhow!("Failed to connect to the network"))?;

        let (sender, receiver) = tokio::sync::mpsc::channel(10);
        let client = self.client().context("Failed to fetch best block")?;

        let (target, status) = match backoff::future::retry(backoff.clone(), || {
            self.network.status().map(|result_status| match result_status?.sync_state {
                SyncState::Idle => Err(backoff::Error::transient(())),
                SyncState::Importing { target } => Ok((target, SyncStatus::Importing)),
                SyncState::Downloading { target } => Ok((target, SyncStatus::Downloading)),
            })
        })
        .await
        {
            Ok((target, status)) => (target, status),
            Err(()) =>
                return Ok(SyncingProgressStream {
                    inner: tokio_stream::wrappers::ReceiverStream::new(receiver),
                    at: 0,
                    target: 0,
                }),
        };

        let at = client.chain_info().best_number;
        let stream = SyncingProgressStream {
            inner: tokio_stream::wrappers::ReceiverStream::new(receiver),
            at,
            target,
        };
        sender
            .send(SyncingProgress { target, at, status })
            .await
            .expect("We are holding receiver, so it will never panic");

        tokio::spawn({
            let network = Arc::clone(&self.network);
            async move {
                loop {
                    tokio::time::sleep(CHECK_SYNCED_EVERY).await;
                    let status = backoff::future::retry(backoff.clone(), || {
                        network.status().map(|result_status| match result_status?.sync_state {
                            SyncState::Idle => Err(backoff::Error::transient(())),
                            SyncState::Importing { target } => Ok((target, SyncStatus::Importing)),
                            SyncState::Downloading { target } =>
                                Ok((target, SyncStatus::Downloading)),
                        })
                    })
                    .await;

                    match status {
                        Ok((target, status)) => {
                            let at = client.chain_info().best_number;
                            if sender.send(SyncingProgress { target, at, status }).await.is_err() {
                                break;
                            }
                        }
                        Err(()) => break,
                    }
                }
            }
        });

        Ok(stream)
    }

    /// Wait till the end of node syncing
    pub async fn sync(&self) -> anyhow::Result<()> {
        self.subscribe_syncing_progress().await?.for_each(|_| async move {}).await;
        Ok(())
    }

    /// Leaves the network and gracefully shuts down
    pub async fn close(mut self) {
        let (stop_sender, stop_receiver) = oneshot::channel();
        drop(self.stop_sender.send(stop_sender).await);
        let _ = stop_receiver.await;
    }

    /// Tells if the node was closed
    pub async fn is_closed(&self) -> bool {
        self.stop_sender.is_closed()
    }

    /// Runs `.close()` and also wipes node's state
    pub async fn wipe(path: impl AsRef<Path>) -> io::Result<()> {
        tokio::fs::remove_dir_all(path).await
    }

    fn client(&self) -> anyhow::Result<Arc<FullClient<RuntimeApi, ExecutorDispatch>>> {
        self.client.upgrade().ok_or_else(|| anyhow::anyhow!("The node was already closed"))
    }

    /// Get node info
    pub async fn get_info(&self) -> anyhow::Result<Info> {
        let SlotInfo { solution_range, voting_solution_range, .. } = self
            .subscribe_slot_info()
            .await
            .map_err(anyhow::Error::msg)?
            .next()
            .await
            .expect("This stream never ends");
        let version = self.rpc_handle.call("state_getRuntimeVersion", &[] as &[()]).await?;
        let client = self.client().context("Failed to fetch node info")?;
        let NetworkState { connected_peers, not_connected_peers, .. } =
            self.network.network_state().await.unwrap();
        let sp_blockchain::Info {
            best_hash,
            best_number,
            genesis_hash,
            finalized_hash,
            finalized_number,
            block_gap,
            ..
        } = client.chain_info();
        let FarmerProtocolInfo { total_pieces, .. } =
            self.farmer_app_info().await.map_err(anyhow::Error::msg)?.protocol_info;
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

fn subscription_to_stream<T: serde::de::DeserializeOwned>(
    mut subscription: jsonrpsee_core::server::rpc_module::Subscription,
) -> impl Stream<Item = T> + Unpin {
    futures::stream::poll_fn(move |cx| {
        Box::pin(subscription.next()).poll_unpin(cx).map(|x| x.and_then(Result::ok).map(|(x, _)| x))
    })
}

mod farmer_rpc_client {
    use std::pin::Pin;

    use futures::Stream;
    use subspace_archiving::archiver::ArchivedSegment;
    use subspace_core_primitives::{RecordsRoot, SegmentIndex};
    use subspace_farmer::rpc_client::{Error, RpcClient};
    use subspace_rpc_primitives::{
        FarmerAppInfo, RewardSignatureResponse, RewardSigningInfo, SlotInfo, SolutionResponse,
    };

    use super::*;

    #[async_trait::async_trait]
    impl RpcClient for Node {
        async fn farmer_app_info(&self) -> Result<FarmerAppInfo, Error> {
            Ok(self.rpc_handle.call("subspace_getFarmerAppInfo", &[] as &[()]).await?)
        }

        async fn subscribe_slot_info(
            &self,
        ) -> Result<Pin<Box<dyn Stream<Item = SlotInfo> + Send + 'static>>, Error> {
            Ok(Box::pin(subscription_to_stream(
                self.rpc_handle.subscribe("subspace_subscribeSlotInfo", &[] as &[()]).await?,
            )))
        }

        async fn submit_solution_response(
            &self,
            solution_response: SolutionResponse,
        ) -> Result<(), Error> {
            Ok(self.rpc_handle.call("subspace_submitSolutionResponse", [solution_response]).await?)
        }

        async fn subscribe_reward_signing(
            &self,
        ) -> Result<Pin<Box<dyn Stream<Item = RewardSigningInfo> + Send + 'static>>, Error>
        {
            Ok(Box::pin(subscription_to_stream(
                self.rpc_handle.subscribe("subspace_subscribeRewardSigning", &[] as &[()]).await?,
            )))
        }

        async fn submit_reward_signature(
            &self,
            reward_signature: RewardSignatureResponse,
        ) -> Result<(), Error> {
            Ok(self.rpc_handle.call("subspace_submitRewardSignature", [reward_signature]).await?)
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
            Ok(self.rpc_handle.call("subspace_recordsRoots", [segment_indexes]).await?)
        }
    }
}

#[cfg(test)]
mod tests {
    use subspace_farmer::RpcClient;
    use tempfile::TempDir;

    use super::*;
    use crate::farmer::CacheDescription;
    use crate::{Farmer, PlotDescription};

    #[tokio::test(flavor = "multi_thread")]
    async fn test_start_node() {
        let dir = TempDir::new().unwrap();
        Node::builder().build(dir.path(), chain_spec::dev_config().unwrap()).await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_rpc() {
        let dir = TempDir::new().unwrap();
        let node =
            Node::builder().build(dir.path(), chain_spec::dev_config().unwrap()).await.unwrap();

        assert!(node.farmer_app_info().await.is_ok());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_closing() {
        let dir = TempDir::new().unwrap();
        let node = Node::builder()
            .force_authoring(true)
            .role(Role::Authority)
            .build(dir.path(), chain_spec::dev_config().unwrap())
            .await
            .unwrap();
        let (plot_dir, cache_dir) = (TempDir::new().unwrap(), TempDir::new().unwrap());
        let plots = [PlotDescription::new(plot_dir.as_ref(), PlotDescription::MIN_SIZE).unwrap()];
        let farmer = Farmer::builder()
            .build(
                Default::default(),
                node.clone(),
                &plots,
                CacheDescription::new(cache_dir.as_ref(), CacheDescription::MIN_SIZE).unwrap(),
            )
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;

        farmer.close().await.unwrap();
        node.close().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "Works most of times though"]
    async fn test_sync_block() {
        let dir = TempDir::new().unwrap();
        let chain = chain_spec::dev_config().unwrap();
        let node = Node::builder()
            .force_authoring(true)
            .network(
                NetworkBuilder::new()
                    .force_synced(true)
                    .listen_addresses(vec!["/ip4/127.0.0.1/tcp/0".parse().unwrap()]),
            )
            .role(Role::Authority)
            .build(dir.path(), chain.clone())
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

        let farm_blocks = 4;

        node.subscribe_new_blocks()
            .await
            .unwrap()
            .skip_while(|notification| futures::future::ready(notification.number < farm_blocks))
            .next()
            .await
            .unwrap();

        farmer.close().await.unwrap();

        let dir = TempDir::new().unwrap();
        let other_node = Node::builder()
            .force_authoring(true)
            .role(Role::Authority)
            .network(NetworkBuilder::new().boot_nodes(node.listen_addresses().await.unwrap()))
            .build(dir.path(), chain)
            .await
            .unwrap();

        other_node.subscribe_syncing_progress().await.unwrap().for_each(|_| async {}).await;
        assert_eq!(other_node.get_info().await.unwrap().best_block.1, farm_blocks);

        node.close().await;
        other_node.close().await;
    }
}

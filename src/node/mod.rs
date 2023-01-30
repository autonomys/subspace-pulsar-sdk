use std::io;
use std::num::{NonZeroU64, NonZeroUsize};
use std::path::Path;
use std::sync::{Arc, Weak};
use std::time::Duration;

use anyhow::Context;
use derivative::Derivative;
use either::*;
use futures::channel::{mpsc, oneshot};
use futures::{FutureExt, SinkExt, Stream, StreamExt};
use libp2p_core::Multiaddr;
use sc_executor::{WasmExecutionMethod, WasmtimeInstantiationStrategy};
use sc_network::config::{NodeKeyConfig, Secret};
use sc_network::network_state::NetworkState;
use sc_network::{NetworkService, NetworkStateInfo, NetworkStatusProvider, SyncState};
use sc_network_common::config::{MultiaddrWithPeerId, TransportConfig};
use sc_rpc_api::state::StateApiClient;
use sc_service::config::{KeystoreConfig, NetworkConfiguration, OffchainWorkerConfig};
use sc_service::{BasePath, Configuration, DatabaseSource, TracingReceiver};
use serde::{Deserialize, Serialize};
use sp_consensus::SyncOracle;
use sp_core::H256;
use subspace_core_primitives::SolutionRange;
use subspace_farmer::node_client::NodeClient;
use subspace_farmer::utils::parity_db_store::ParityDbStore;
use subspace_farmer::utils::readers_and_pieces::ReadersAndPieces;
use subspace_farmer_components::FarmerProtocolInfo;
use subspace_networking::{PieceByHashRequest, PieceByHashRequestHandler, PieceByHashResponse};
use subspace_rpc_primitives::SlotInfo;
use subspace_runtime::RuntimeApi;
use subspace_runtime_primitives::opaque::{Block as RuntimeBlock, Header};
use subspace_service::piece_cache::PieceCache;
use subspace_service::SubspaceConfiguration;

use self::builder::SegmentPublishConcurrency;
use crate::networking::provider_storage_utils::MaybeProviderStorage;
use crate::networking::{FarmerProviderStorage, NodeProviderStorage, ProviderStorage};

pub mod chain_spec;
pub mod domains;

pub use builder::{
    Base, BaseBuilder, BlocksPruning, Builder, Config, Constraints, Dsn, DsnBuilder,
    ExecutionStrategy, Network, NetworkBuilder, OffchainWorker, OffchainWorkerBuilder, PruningMode,
    Rpc, RpcBuilder,
};
pub(crate) use builder::{ImplName, ImplVersion};
pub use domains::{ConfigBuilder as SystemDomainBuilder, SystemDomainNode};

mod builder {
    use std::net::SocketAddr;
    use std::num::NonZeroUsize;

    use derivative::Derivative;
    use derive_builder::Builder;
    use derive_more::{Deref, DerefMut, Display, From};
    use sc_network::ProtocolName;
    use sc_network_common::config::NonDefaultSetConfig;
    use serde::{Deserialize, Serialize};

    use super::*;

    /// Block pruning settings.
    #[derive(
        Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize, Eq, PartialOrd, Ord,
    )]
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
    }

    impl From<Constraints> for sc_state_db::Constraints {
        fn from(Constraints { max_blocks }: Constraints) -> Self {
            Self { max_blocks }
        }
    }

    impl From<sc_state_db::Constraints> for Constraints {
        fn from(sc_state_db::Constraints { max_blocks }: sc_state_db::Constraints) -> Self {
            Self { max_blocks }
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

    #[derive(
        Debug,
        Clone,
        Derivative,
        Deserialize,
        Serialize,
        PartialEq,
        Eq,
        From,
        Deref,
        DerefMut,
        Display,
    )]
    #[derivative(Default)]
    #[serde(transparent)]
    pub struct PieceCacheSize(
        #[derivative(Default(value = "bytesize::ByteSize::gib(1)"))]
        #[serde(with = "bytesize_serde")]
        pub(crate) bytesize::ByteSize,
    );

    #[derive(
        Debug,
        Clone,
        Derivative,
        Deserialize,
        Serialize,
        PartialEq,
        Eq,
        From,
        Deref,
        DerefMut,
        Display,
    )]
    #[derivative(Default)]
    #[serde(transparent)]
    pub struct SegmentPublishConcurrency(
        #[derivative(Default(value = "NonZeroUsize::new(10).expect(\"10 > 0\")"))]
        pub(crate)  NonZeroUsize,
    );

    /// Node builder
    #[derive(Debug, Clone, Derivative, Builder, Deserialize, Serialize, PartialEq)]
    #[derivative(Default)]
    #[builder(pattern = "immutable", build_fn(private, name = "_build"), name = "Builder")]
    #[non_exhaustive]
    pub struct Config {
        /// Set piece cache size
        #[builder(setter(into), default)]
        #[serde(default, skip_serializing_if = "crate::utils::is_default")]
        pub piece_cache_size: PieceCacheSize,
        /// Max number of segments that can be published concurrently, impacts
        /// RAM usage and network bandwidth.
        #[builder(setter(into), default)]
        #[serde(default, skip_serializing_if = "crate::utils::is_default")]
        pub segment_publish_concurrency: SegmentPublishConcurrency,
        /// Should we sync blocks from the DSN?
        #[builder(default)]
        #[serde(default, skip_serializing_if = "crate::utils::is_default")]
        pub sync_from_dsn: bool,
        #[doc(hidden)]
        #[builder(
            setter(into, strip_option),
            field(type = "BaseBuilder", build = "self.base.build()")
        )]
        #[serde(flatten, skip_serializing_if = "crate::utils::is_default")]
        pub base: Base,
        /// System domain settings
        #[builder(setter(into, strip_option), default)]
        #[serde(default, skip_serializing_if = "crate::utils::is_default")]
        pub system_domain: Option<domains::Config>,
        /// DSN settings
        #[builder(setter(into), default)]
        #[serde(default, skip_serializing_if = "crate::utils::is_default")]
        pub dsn: Dsn,
    }

    impl Config {
        /// Dev configuraiton
        pub fn dev() -> Builder {
            Builder::dev()
        }

        /// Gemini 3c configuraiton
        pub fn gemini_3c() -> Builder {
            Builder::gemini_3c()
        }

        /// Devnet configuraiton
        pub fn devnet() -> Builder {
            Builder::devnet()
        }
    }

    #[derive(
        Debug,
        Clone,
        Derivative,
        Deserialize,
        Serialize,
        PartialEq,
        Eq,
        From,
        Deref,
        DerefMut,
        Display,
    )]
    #[derivative(Default)]
    #[serde(transparent)]
    pub struct ImplName(
        #[derivative(Default(value = "env!(\"CARGO_PKG_NAME\").to_owned()"))] pub(crate) String,
    );

    #[derive(
        Debug,
        Clone,
        Derivative,
        Deserialize,
        Serialize,
        PartialEq,
        Eq,
        From,
        Deref,
        DerefMut,
        Display,
    )]
    #[derivative(Default)]
    #[serde(transparent)]
    pub struct ImplVersion(
        #[derivative(Default(
            value = "format!(\"{}-{}\", env!(\"CARGO_PKG_VERSION\"), env!(\"GIT_HASH\"))"
        ))]
        pub(crate) String,
    );

    #[doc(hidden)]
    #[derive(Debug, Clone, Derivative, Builder, Deserialize, Serialize, PartialEq)]
    #[derivative(Default)]
    #[builder(pattern = "immutable", build_fn(private, name = "_build"), name = "BaseBuilder")]
    #[non_exhaustive]
    pub struct Base {
        /// Force block authoring
        #[builder(default)]
        #[serde(default, skip_serializing_if = "crate::utils::is_default")]
        pub force_authoring: bool,
        /// Set node role
        #[builder(default)]
        #[serde(default, skip_serializing_if = "crate::utils::is_default")]
        pub role: Role,
        /// Blocks pruning options
        #[builder(default)]
        #[serde(default, skip_serializing_if = "crate::utils::is_default")]
        pub blocks_pruning: BlocksPruning,
        /// State pruning options
        #[builder(default)]
        #[serde(default, skip_serializing_if = "crate::utils::is_default")]
        pub state_pruning: PruningMode,
        /// Set execution strategies
        #[builder(default)]
        #[serde(default, skip_serializing_if = "crate::utils::is_default")]
        pub execution_strategy: ExecutionStrategy,
        /// Implementation name
        #[builder(default)]
        #[serde(default, skip_serializing_if = "crate::utils::is_default")]
        pub impl_name: ImplName,
        /// Implementation version
        #[builder(default)]
        #[serde(default, skip_serializing_if = "crate::utils::is_default")]
        pub impl_version: ImplVersion,
        /// Rpc settings
        #[builder(setter(into), default)]
        #[serde(default, skip_serializing_if = "crate::utils::is_default")]
        pub rpc: Rpc,
        /// Network settings
        #[builder(setter(into), default)]
        #[serde(default, skip_serializing_if = "crate::utils::is_default")]
        pub network: Network,
        /// Offchain worker settings
        #[builder(setter(into), default)]
        #[serde(default, skip_serializing_if = "crate::utils::is_default")]
        pub offchain_worker: OffchainWorker,
    }

    #[doc(hidden)]
    #[macro_export]
    macro_rules! derive_base {
        (
            $base:ty => $builder:ident {
                $(
                    #[doc = $doc:literal]
                    $field:ident : $field_ty:ty
                ),+
                $(,)?
            }
        ) => {
            impl $builder {
                $(
                #[doc = $doc]
                pub fn $field(&self, $field: impl Into<$field_ty>) -> Self {
                    let mut me = self.clone();
                    me.base = me.base.$field($field.into());
                    me
                }
                )*
            }
        };
        ( $base:ty => $builder:ident ) => {
            $crate::derive_base!($base => $builder {
                /// Force block authoring
                force_authoring: bool,
                /// Set node role
                role: $crate::node::Role,
                /// Blocks pruning options
                blocks_pruning: $crate::node::BlocksPruning,
                /// State pruning options
                state_pruning: $crate::node::PruningMode,
                /// Set execution strategies
                execution_strategy: $crate::node::ExecutionStrategy,
                /// Implementation name
                impl_name: $crate::node::ImplName,
                /// Implementation version
                impl_version: $crate::node::ImplVersion,
                /// Rpc settings
                rpc: $crate::node::Rpc,
                /// Network settings
                network: $crate::node::Network,
                /// Offchain worker settings
                offchain_worker: $crate::node::OffchainWorker,
            });
        }
    }

    impl Base {
        pub(crate) async fn configuration<CS>(
            self,
            directory: impl AsRef<Path>,
            chain_spec: CS,
        ) -> Configuration
        where
            CS: sc_chain_spec::ChainSpec
                + serde::Serialize
                + serde::de::DeserializeOwned
                + sp_runtime::BuildStorage
                + 'static,
        {
            const NODE_KEY_ED25519_FILE: &str = "secret_ed25519";
            const DEFAULT_NETWORK_CONFIG_PATH: &str = "network";

            let Self {
                force_authoring,
                role,
                blocks_pruning,
                state_pruning,
                execution_strategy,
                impl_name: ImplName(impl_name),
                impl_version: ImplVersion(impl_version),
                rpc:
                    Rpc {
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
                offchain_worker,
            } = self;

            let base_path = BasePath::new(directory.as_ref());
            let config_dir = base_path.config_dir(chain_spec.id());

            let mut network = {
                let builder::Network {
                    listen_addresses,
                    boot_nodes,
                    force_synced,
                    name,
                    client_id,
                    enable_mdns,
                    allow_private_ipv4,
                    allow_non_globals_in_dht,
                } = network;
                let name = name.unwrap_or_else(|| {
                    names::Generator::with_naming(names::Name::Numbered)
                        .next()
                        .filter(|name| name.chars().count() < NODE_NAME_MAX_LENGTH)
                        .expect("RNG is available on all supported platforms; qed")
                });

                let client_id = client_id.unwrap_or_else(|| format!("{impl_name}/v{impl_version}"));
                let config_dir = config_dir.join(DEFAULT_NETWORK_CONFIG_PATH);
                let listen_addresses = listen_addresses
                    .into_iter()
                    .map(|addr| {
                        addr.to_string()
                            .parse()
                            .expect("Conversion between 2 libp2p versions is always right")
                    })
                    .collect::<Vec<_>>();

                NetworkConfiguration {
                    listen_addresses,
                    boot_nodes: chain_spec.boot_nodes().iter().cloned().chain(boot_nodes).collect(),
                    force_synced,
                    transport: TransportConfig::Normal { enable_mdns, allow_private_ipv4 },
                    extra_sets: vec![NonDefaultSetConfig::new(
                        ProtocolName::Static("/subspace/cross-domain-messages"),
                        40,
                    )],
                    allow_non_globals_in_dht,
                    ..NetworkConfiguration::new(
                        name,
                        client_id,
                        NodeKeyConfig::Ed25519(Secret::File(
                            config_dir.join(NODE_KEY_ED25519_FILE),
                        )),
                        Some(config_dir),
                    )
                }
            };

            // Increase default value of 25 to improve success rate of sync
            network.default_peers_set.out_peers = 50;
            // Full + Light clients
            network.default_peers_set.in_peers = 25 + 100;
            let (keystore_remote, keystore) = (None, KeystoreConfig::InMemory);
            let telemetry_endpoints = chain_spec.telemetry_endpoints().clone();

            Configuration {
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
            }
        }
    }

    #[derive(
        Debug,
        Clone,
        Derivative,
        Deserialize,
        Serialize,
        PartialEq,
        Eq,
        From,
        Deref,
        DerefMut,
        Display,
    )]
    #[derivative(Default)]
    #[serde(transparent)]
    pub struct MaxSubsPerConn(#[derivative(Default(value = "1024"))] pub(crate) usize);

    /// Node RPC builder
    #[derive(Debug, Clone, Derivative, Builder, Deserialize, Serialize, PartialEq, Eq)]
    #[derivative(Default)]
    #[builder(pattern = "immutable", build_fn(private, name = "_build"), name = "RpcBuilder")]
    #[non_exhaustive]
    pub struct Rpc {
        /// RPC over HTTP binding address. `None` if disabled.
        #[builder(setter(strip_option), default)]
        #[serde(default, skip_serializing_if = "crate::utils::is_default")]
        pub http: Option<SocketAddr>,
        /// RPC over Websockets binding address. `None` if disabled.
        #[builder(setter(strip_option), default)]
        #[serde(default, skip_serializing_if = "crate::utils::is_default")]
        pub ws: Option<SocketAddr>,
        /// RPC over IPC binding path. `None` if disabled.
        #[builder(setter(strip_option), default)]
        #[serde(default, skip_serializing_if = "crate::utils::is_default")]
        pub ipc: Option<String>,
        /// Maximum number of connections for WebSockets RPC server. `None` if
        /// default.
        #[builder(setter(strip_option), default)]
        #[serde(default, skip_serializing_if = "crate::utils::is_default")]
        pub ws_max_connections: Option<usize>,
        /// CORS settings for HTTP & WS servers. `None` if all origins are
        /// allowed.
        #[builder(setter(strip_option), default)]
        #[serde(default, skip_serializing_if = "crate::utils::is_default")]
        pub cors: Option<Vec<String>>,
        /// RPC methods to expose (by default only a safe subset or all of
        /// them).
        #[builder(default)]
        #[serde(default, skip_serializing_if = "crate::utils::is_default")]
        pub methods: RpcMethods,
        /// Maximum payload of rpc request/responses.
        #[builder(setter(strip_option), default)]
        #[serde(default, skip_serializing_if = "crate::utils::is_default")]
        pub max_payload: Option<usize>,
        /// Maximum payload of a rpc request
        #[builder(setter(strip_option), default)]
        #[serde(default, skip_serializing_if = "crate::utils::is_default")]
        pub max_request_size: Option<usize>,
        /// Maximum payload of a rpc request
        #[builder(setter(strip_option), default)]
        #[serde(default, skip_serializing_if = "crate::utils::is_default")]
        pub max_response_size: Option<usize>,
        /// Maximum allowed subscriptions per rpc connection
        #[builder(default)]
        #[serde(default, skip_serializing_if = "crate::utils::is_default")]
        pub max_subs_per_conn: usize,
        /// Maximum size of the output buffer capacity for websocket
        /// connections.
        #[builder(setter(strip_option), default)]
        #[serde(default, skip_serializing_if = "crate::utils::is_default")]
        pub ws_max_out_buffer_capacity: Option<usize>,
    }

    impl RpcBuilder {
        /// Dev configuration
        pub fn dev() -> Self {
            Self::default()
        }

        /// Gemini 3c configuration
        pub fn gemini_3c() -> Self {
            Self::new()
                .http("127.0.0.1:9933".parse().expect("hardcoded value is true"))
                .ws("127.0.0.1:9944".parse().expect("hardcoded value is true"))
                .cors(vec![
                    "http://localhost:*".to_owned(),
                    "http://127.0.0.1:*".to_owned(),
                    "https://localhost:*".to_owned(),
                    "https://127.0.0.1:*".to_owned(),
                    "https://polkadot.js.org".to_owned(),
                ])
        }

        /// Devnet configuration
        pub fn devnet() -> Self {
            Self::new()
                .http("127.0.0.1:9933".parse().expect("hardcoded value is true"))
                .ws("127.0.0.1:9944".parse().expect("hardcoded value is true"))
                .cors(vec![
                    "http://localhost:*".to_owned(),
                    "http://127.0.0.1:*".to_owned(),
                    "https://localhost:*".to_owned(),
                    "https://127.0.0.1:*".to_owned(),
                    "https://polkadot.js.org".to_owned(),
                ])
        }
    }

    /// Node network builder
    #[derive(Debug, Default, Clone, Builder, Deserialize, Serialize, PartialEq)]
    #[builder(pattern = "immutable", build_fn(private, name = "_build"), name = "NetworkBuilder")]
    #[non_exhaustive]
    pub struct Network {
        /// Listen on some address for other nodes
        #[builder(default)]
        #[serde(default, skip_serializing_if = "crate::utils::is_default")]
        pub enable_mdns: bool,
        /// Listen on some address for other nodes
        #[builder(default)]
        #[serde(default, skip_serializing_if = "crate::utils::is_default")]
        pub allow_private_ipv4: bool,
        /// Allow non globals in network DHT
        #[builder(default)]
        #[serde(default, skip_serializing_if = "crate::utils::is_default")]
        pub allow_non_globals_in_dht: bool,
        /// Listen on some address for other nodes
        #[builder(default)]
        #[serde(default, skip_serializing_if = "crate::utils::is_default")]
        pub listen_addresses: Vec<Multiaddr>,
        /// Boot nodes
        #[builder(default)]
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub boot_nodes: Vec<MultiaddrWithPeerId>,
        /// Force node to think it is synced
        #[builder(default)]
        #[serde(default, skip_serializing_if = "crate::utils::is_default")]
        pub force_synced: bool,
        /// Node name
        #[builder(setter(into, strip_option), default)]
        #[serde(default, skip_serializing_if = "crate::utils::is_default")]
        pub name: Option<String>,
        /// Client id for telemetry (default is `{IMPL_NAME}/v{IMPL_VERSION}`)
        #[builder(setter(into, strip_option), default)]
        #[serde(default, skip_serializing_if = "crate::utils::is_default")]
        pub client_id: Option<String>,
    }

    impl NetworkBuilder {
        /// Dev chain configuration
        pub fn dev() -> Self {
            Self::default().force_synced(true).allow_private_ipv4(true)
        }

        /// Gemini 3c configuration
        pub fn gemini_3c() -> Self {
            Self::default()
                .listen_addresses(vec![
                    "/ip6/::/tcp/30333".parse().expect("hardcoded value is true"),
                    "/ip4/0.0.0.0/tcp/30333".parse().expect("hardcoded value is true"),
                ])
                .enable_mdns(true)
        }

        /// Gemini 3c configuration
        pub fn devnet() -> Self {
            Self::default()
                .listen_addresses(vec![
                    "/ip6/::/tcp/30333".parse().expect("hardcoded value is true"),
                    "/ip4/0.0.0.0/tcp/30333".parse().expect("hardcoded value is true"),
                ])
                .enable_mdns(true)
        }
    }

    #[derive(
        Debug, Clone, Derivative, Deserialize, Serialize, PartialEq, Eq, From, Deref, DerefMut,
    )]
    #[derivative(Default)]
    #[serde(transparent)]
    pub struct ListenAddresses(
        #[derivative(Default(
            // TODO: get rid of it, once it won't be required by monorepo
            value = "vec![\"/ip4/127.0.0.1/tcp/0\".parse().expect(\"Always valid\")]"
        ))]
        pub(crate) Vec<libp2p_core::Multiaddr>,
    );

    /// Node DSN builder
    #[derive(Debug, Clone, Derivative, Builder, Deserialize, Serialize, PartialEq, Eq)]
    #[derivative(Default)]
    #[builder(pattern = "immutable", build_fn(private, name = "_build"), name = "DsnBuilder")]
    #[non_exhaustive]
    pub struct Dsn {
        /// Listen on some address for other nodes
        #[builder(default, setter(into, strip_option))]
        #[serde(default, skip_serializing_if = "crate::utils::is_default")]
        pub provider_storage_path: Option<std::path::PathBuf>,
        /// Listen on some address for other nodes
        #[builder(default, setter(into))]
        #[serde(default, skip_serializing_if = "crate::utils::is_default")]
        pub listen_addresses: ListenAddresses,
        /// Boot nodes
        #[builder(default)]
        #[serde(default, skip_serializing_if = "crate::utils::is_default")]
        pub boot_nodes: Vec<libp2p_core::Multiaddr>,
        /// Reserved nodes
        #[builder(default)]
        #[serde(default, skip_serializing_if = "crate::utils::is_default")]
        pub reserved_nodes: Vec<libp2p_core::Multiaddr>,
        /// Determines whether we allow keeping non-global (private, shared,
        /// loopback..) addresses in Kademlia DHT.
        #[builder(default)]
        #[serde(default, skip_serializing_if = "crate::utils::is_default")]
        pub allow_non_global_addresses_in_dht: bool,
    }

    impl DsnBuilder {
        /// Dev chain configuration
        pub fn dev() -> Self {
            Self::new().allow_non_global_addresses_in_dht(true)
        }

        /// Gemini 3c configuration
        pub fn gemini_3c() -> Self {
            Self::new().listen_addresses(vec![
                "/ip6/::/tcp/30433".parse().expect("hardcoded value is true"),
                "/ip4/0.0.0.0/tcp/30433".parse().expect("hardcoded value is true"),
            ])
        }

        /// Gemini 3c configuration
        pub fn devnet() -> Self {
            Self::new().listen_addresses(vec![
                "/ip6/::/tcp/30433".parse().expect("hardcoded value is true"),
                "/ip4/0.0.0.0/tcp/30433".parse().expect("hardcoded value is true"),
            ])
        }
    }

    /// Offchain worker config
    #[derive(Debug, Clone, Derivative, Builder, Deserialize, Serialize, PartialEq, Eq)]
    #[derivative(Default)]
    #[builder(pattern = "immutable", build_fn(name = "_build"), name = "OffchainWorkerBuilder")]
    #[non_exhaustive]
    pub struct OffchainWorker {
        /// Is enabled
        #[builder(default)]
        #[serde(default, skip_serializing_if = "crate::utils::is_default")]
        pub enabled: bool,
        /// Is indexing enabled
        #[builder(default)]
        #[serde(default, skip_serializing_if = "crate::utils::is_default")]
        pub indexing_enabled: bool,
    }

    impl OffchainWorkerBuilder {
        /// Dev chain configuration
        pub fn dev() -> Self {
            Self::default()
        }

        /// Gemini 3c configuration
        pub fn gemini_3c() -> Self {
            Self::default().enabled(true)
        }

        /// Devnet configuration
        pub fn devnet() -> Self {
            Self::default().enabled(true)
        }
    }

    impl From<OffchainWorker> for OffchainWorkerConfig {
        fn from(OffchainWorker { enabled, indexing_enabled }: OffchainWorker) -> Self {
            Self { enabled, indexing_enabled }
        }
    }

    impl Builder {
        /// Dev chain configuration
        pub fn dev() -> Self {
            Self::new()
                .force_authoring(true)
                .network(NetworkBuilder::dev())
                .dsn(DsnBuilder::dev())
                .rpc(RpcBuilder::dev())
                .offchain_worker(OffchainWorkerBuilder::dev())
        }

        /// Gemini 3c configuration
        pub fn gemini_3c() -> Self {
            Self::new()
                .execution_strategy(ExecutionStrategy::AlwaysWasm)
                .network(NetworkBuilder::gemini_3c())
                .dsn(DsnBuilder::gemini_3c())
                .rpc(RpcBuilder::gemini_3c())
                .offchain_worker(OffchainWorkerBuilder::gemini_3c())
        }

        /// Devnet chain configuration
        pub fn devnet() -> Self {
            Self::new()
                .execution_strategy(ExecutionStrategy::AlwaysWasm)
                .network(NetworkBuilder::devnet())
                .dsn(DsnBuilder::devnet())
                .rpc(RpcBuilder::devnet())
                .offchain_worker(OffchainWorkerBuilder::devnet())
        }

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
            chain_spec: super::ChainSpec,
        ) -> anyhow::Result<Node> {
            self.configuration().build(directory, chain_spec).await
        }
    }

    crate::derive_base!(Base => Builder);
    crate::generate_builder!(Base, Rpc, Network, Dsn, OffchainWorker);
}

/// Role of the local node.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
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
#[derive(Debug, Copy, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
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
const MAX_PROVIDER_RECORDS_LIMIT: NonZeroUsize = NonZeroUsize::new(100000).expect("100000 > 0"); // ~ 10 MB

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
            dsn,
            system_domain,
            segment_publish_concurrency: SegmentPublishConcurrency(segment_publish_concurrency),
            sync_from_dsn,
        } = self;
        let base = base.configuration(directory.as_ref(), chain_spec.clone()).await;
        let name = base.network.node_name.clone();

        let partial_components =
            subspace_service::new_partial::<RuntimeApi, ExecutorDispatch>(&base)
                .context("Failed to build a partial subspace node")?;
        let farmer_readers_and_pieces = Arc::new(parking_lot::Mutex::new(None));
        let farmer_piece_store = Arc::new(parking_lot::Mutex::new(None));
        let farmer_provider_storage = MaybeProviderStorage::none();

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

            let piece_cache = PieceCache::new(
                partial_components.client.clone(),
                piece_cache_size.as_u64() / subspace_core_primitives::PIECE_SIZE as u64,
                subspace_networking::peer_id(&keypair),
            );

            // Start before archiver below, so we don't have potential race condition and
            // miss pieces
            tokio::spawn({
                let mut piece_cache = piece_cache.clone();
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
                            tracing::error!(
                                %segment_index,
                                %error,
                                "Failed to store pieces for segment in cache"
                            );
                        }
                    }
                }
            });

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

                let builder::Dsn {
                    listen_addresses,
                    boot_nodes,
                    reserved_nodes,
                    allow_non_global_addresses_in_dht,
                    provider_storage_path,
                } = dsn;

                let peer_id = subspace_networking::peer_id(&keypair);
                let bootstrap_nodes = chain_spec_boot_nodes
                    .into_iter()
                    .chain(boot_nodes)
                    .map(|a| {
                        a.to_string()
                            .parse()
                            .expect("Convertion between 2 libp2p version. Never panics")
                    })
                    .collect::<Vec<_>>();

                let listen_on = listen_addresses
                    .0
                    .into_iter()
                    .map(|a| {
                        a.to_string()
                            .parse()
                            .expect("Convertion between 2 libp2p version. Never panics")
                    })
                    .collect();

                let external_provider_storage = match provider_storage_path {
                    Some(path) => Either::Left(subspace_networking::ParityDbProviderStorage::new(
                        &path,
                        MAX_PROVIDER_RECORDS_LIMIT,
                        peer_id,
                    )?),
                    None => Either::Right(subspace_networking::MemoryProviderStorage::new(peer_id)),
                };

                let node_provider_storage =
                    NodeProviderStorage::new(piece_cache.clone(), external_provider_storage);
                let provider_storage =
                    ProviderStorage::new(farmer_provider_storage.clone(), node_provider_storage);

                let networking_config = subspace_networking::Config {
                    keypair: keypair.clone(),
                    listen_on,
                    allow_non_global_addresses_in_dht,
                    networking_parameters_registry:
                        subspace_networking::BootstrappedNetworkingParameters::new(
                            bootstrap_nodes.clone(),
                        )
                        .boxed(),
                    request_response_protocols: vec![PieceByHashRequestHandler::create({
                        let handle = tokio::runtime::Handle::current();
                        let weak_readers_and_pieces = Arc::downgrade(&farmer_readers_and_pieces);
                        let farmer_piece_store = Arc::clone(&farmer_piece_store);
                        let piece_cache = piece_cache.clone();

                        move |req| {
                            match get_piece_by_hash(req, &piece_cache) {
                                Some(PieceByHashResponse { piece: None }) | None => (),
                                result => return result,
                            }

                            if let Some(piece_store) = farmer_piece_store.lock().as_ref() {
                                crate::farmer::get_piece_by_hash(
                                    req,
                                    piece_store,
                                    &weak_readers_and_pieces,
                                    &handle,
                                )
                            } else {
                                None
                            }
                        }
                    })],
                    provider_storage,
                    reserved_peers: reserved_nodes
                        .into_iter()
                        .map(|addr| {
                            addr.to_string()
                                .parse()
                                .expect("Conversion between 2 libp2p versions is always right")
                        })
                        .collect(),
                    ..subspace_networking::Config::default()
                };

                subspace_networking::create(networking_config)
                    .await
                    .map(|(a, b)| (a, b, bootstrap_nodes))?
            };

            tracing::debug!("Subspace networking initialized: Node ID is {}", node.id());

            (
                subspace_service::SubspaceNetworking::Reuse { node: node.clone(), bootstrap_nodes },
                (node, node_runner, piece_cache),
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

        tokio::spawn(async move {
            node_runner.run().await;
        });

        let slot_proportion = sc_consensus_slots::SlotProportion::new(2f32 / 3f32);
        let mut full_client =
            subspace_service::new_full(configuration, partial_components, true, slot_proportion)
                .await
                .context("Failed to build a full subspace node")?;

        let system_domain = if let Some(config) = system_domain {
            use sc_service::ChainSpecExtension;

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

            select_chain: _,
            backend: _,
            reward_signing_notification_stream: _,
            archived_segment_notification_stream: _,
            transaction_pool: _,
            imported_block_notification_stream: _,
            new_slot_notification_stream: _,
        } = full_client;

        let client = Arc::downgrade(&client);
        let rpc_handle = crate::utils::Rpc::new(&rpc_handlers);
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
        })
    }
}

/// Executor dispatch for subspace runtime
pub(crate) struct ExecutorDispatch;

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

/// Chain spec for subspace node
pub type ChainSpec = chain_spec::ChainSpec;
pub(crate) type FullClient =
    subspace_service::FullClient<subspace_runtime::RuntimeApi, ExecutorDispatch>;
pub(crate) type NewFull = subspace_service::NewFull<
    FullClient,
    subspace_service::FraudProofVerifier<RuntimeApi, ExecutorDispatch>,
>;

/// Node structure
#[derive(Clone, Derivative)]
#[derivative(Debug)]
pub struct Node {
    system_domain: Option<SystemDomainNode>,
    #[derivative(Debug = "ignore")]
    client: Weak<FullClient>,
    #[derivative(Debug = "ignore")]
    network: Arc<NetworkService<RuntimeBlock, Hash>>,
    pub(crate) rpc_handle: crate::utils::Rpc,
    stop_sender: mpsc::Sender<oneshot::Sender<()>>,
    pub(crate) dsn_node: subspace_networking::Node,
    name: String,
    pub(crate) farmer_readers_and_pieces: Arc<parking_lot::Mutex<Option<ReadersAndPieces>>>,
    #[derivative(Debug = "ignore")]
    pub(crate) farmer_piece_store: Arc<parking_lot::Mutex<Option<ParityDbStore>>>,
    pub(crate) farmer_provider_storage: MaybeProviderStorage<FarmerProviderStorage>,
    #[derivative(Debug = "ignore")]
    pub(crate) piece_cache: PieceCache<FullClient>,
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

    /// Gemini 3c configuration
    pub fn gemini_3c() -> Builder {
        Builder::gemini_3c()
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
            .with_max_elapsed_time(Some(Duration::from_secs(60)))
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
        let client = self.client().context("Failed to fetch best block")?;
        let inner = tokio_stream::wrappers::ReceiverStream::new(receiver);

        let result = backoff::future::retry(check_synced_backoff.clone(), || {
            self.network.status().map(|result| match result.map(|status| status.sync_state) {
                Ok(SyncState::Importing { target }) => Ok((target, SyncStatus::Importing)),
                Ok(SyncState::Downloading { target }) => Ok((target, SyncStatus::Downloading)),
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

        let at = client.chain_info().best_number;
        sender
            .send(Ok(SyncingProgress { target, at, status }))
            .await
            .expect("We are holding receiver, so it will never panic");

        tokio::spawn({
            let network = Arc::clone(&self.network);
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

    fn client(&self) -> anyhow::Result<Arc<FullClient>> {
        self.client.upgrade().ok_or_else(|| anyhow::anyhow!("The node was already closed"))
    }

    /// Returns system domain node if one was setted up
    pub fn system_domain(&self) -> Option<SystemDomainNode> {
        self.system_domain.as_ref().cloned()
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
        let client = self.client().context("Failed to fetch node info")?;
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
        } = client.chain_info();
        let version = self.rpc_handle.runtime_version(Some(best_hash)).await?;
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
        self.rpc_handle.subscribe_new_blocks().await.context("Failed to subscribe to new blocks")
    }
}

pub(crate) fn get_piece_by_hash(
    PieceByHashRequest { piece_index_hash }: &PieceByHashRequest,
    piece_cache: &subspace_service::piece_cache::PieceCache<impl sc_client_api::AuxStore>,
) -> Option<PieceByHashResponse> {
    let result = match piece_cache.get_piece(*piece_index_hash) {
        Ok(maybe_piece) => maybe_piece,
        Err(error) => {
            tracing::error!(?piece_index_hash, %error, "Failed to get piece from cache");
            None
        }
    };

    Some(PieceByHashResponse { piece: result })
}

mod farmer_rpc_client {
    use std::pin::Pin;

    use futures::Stream;
    use sc_consensus_subspace_rpc::SubspaceRpcApiClient;
    use subspace_archiving::archiver::ArchivedSegment;
    use subspace_core_primitives::{RecordsRoot, SegmentIndex};
    use subspace_farmer::node_client::{Error, NodeClient};
    use subspace_rpc_primitives::{
        FarmerAppInfo, RewardSignatureResponse, RewardSigningInfo, SlotInfo, SolutionResponse,
    };

    use super::*;

    #[async_trait::async_trait]
    impl NodeClient for Node {
        async fn farmer_app_info(&self) -> Result<FarmerAppInfo, Error> {
            Ok(self.rpc_handle.get_farmer_app_info().await?)
        }

        async fn subscribe_slot_info(
            &self,
        ) -> Result<Pin<Box<dyn Stream<Item = SlotInfo> + Send + 'static>>, Error> {
            Ok(Box::pin(
                self.rpc_handle
                    .subscribe_slot_info()
                    .await?
                    .filter_map(|result| futures::future::ready(result.ok())),
            ))
        }

        async fn submit_solution_response(
            &self,
            solution_response: SolutionResponse,
        ) -> Result<(), Error> {
            Ok(self.rpc_handle.submit_solution_response(solution_response).await?)
        }

        async fn subscribe_reward_signing(
            &self,
        ) -> Result<Pin<Box<dyn Stream<Item = RewardSigningInfo> + Send + 'static>>, Error>
        {
            Ok(Box::pin(
                self.rpc_handle
                    .subscribe_reward_signing()
                    .await?
                    .filter_map(|result| futures::future::ready(result.ok())),
            ))
        }

        async fn submit_reward_signature(
            &self,
            reward_signature: RewardSignatureResponse,
        ) -> Result<(), Error> {
            Ok(self.rpc_handle.submit_reward_signature(reward_signature).await?)
        }

        async fn subscribe_archived_segments(
            &self,
        ) -> Result<Pin<Box<dyn Stream<Item = ArchivedSegment> + Send + 'static>>, Error> {
            Ok(Box::pin(
                self.rpc_handle
                    .subscribe_archived_segment()
                    .await?
                    .filter_map(|result| futures::future::ready(result.ok())),
            ))
        }

        async fn records_roots(
            &self,
            segment_indexes: Vec<SegmentIndex>,
        ) -> Result<Vec<Option<RecordsRoot>>, Error> {
            Ok(self.rpc_handle.records_roots(segment_indexes).await?)
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use subspace_farmer::node_client::NodeClient;
    use tempfile::TempDir;

    use super::*;
    use crate::farmer::CacheDescription;
    use crate::{Farmer, PlotDescription};

    #[tokio::test(flavor = "multi_thread")]
    async fn test_start_node() {
        let dir = TempDir::new().unwrap();
        Node::dev()
            .role(Role::Authority)
            .build(dir.path(), chain_spec::dev_config().unwrap())
            .await
            .unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_rpc() {
        let dir = TempDir::new().unwrap();
        let node = Node::dev()
            .role(Role::Authority)
            .build(dir.path(), chain_spec::dev_config().unwrap())
            .await
            .unwrap();

        assert!(node.farmer_app_info().await.is_ok());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_closing() {
        crate::utils::test_init();

        let dir = TempDir::new().unwrap();
        let node = Node::dev()
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
    #[cfg_attr(
        any(tarpaulin, not(target_os = "linux")),
        ignore = "Ignored for coverage tests and not linux platforms"
    )]
    async fn test_sync_block() {
        crate::utils::test_init();

        let dir = TempDir::new().unwrap();
        let chain = chain_spec::dev_config().unwrap();
        let node = Node::dev()
            .role(Role::Authority)
            .network(
                NetworkBuilder::dev()
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
        let other_node = Node::dev()
            .network(
                NetworkBuilder::dev()
                    .force_synced(false)
                    .boot_nodes(node.listen_addresses().await.unwrap()),
            )
            .build(dir.path(), chain)
            .await
            .unwrap();

        other_node.subscribe_syncing_progress().await.unwrap().for_each(|_| async {}).await;
        assert_eq!(other_node.get_info().await.unwrap().best_block.1, farm_blocks);

        node.close().await;
        other_node.close().await;
    }
}

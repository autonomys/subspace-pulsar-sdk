use std::net::SocketAddr;
use std::path::Path;

use derivative::Derivative;
use derive_builder::Builder;
use sc_executor::{WasmExecutionMethod, WasmtimeInstantiationStrategy};
use sc_network::config::{NodeKeyConfig, Secret};
use sc_network::ProtocolName;
use sc_service::config::{
    KeystoreConfig, NetworkConfiguration, NonDefaultSetConfig, TransportConfig,
};
use sc_service::{BasePath, Configuration, DatabaseSource, TracingReceiver};
use sdk_utils::{Multiaddr, MultiaddrWithPeerId};
use serde::{Deserialize, Serialize};
pub use subspace_runtime::RuntimeEvent as Event;
pub use types::*;

mod types;

#[doc(hidden)]
#[derive(Debug, Clone, Derivative, Builder, Deserialize, Serialize, PartialEq)]
#[derivative(Default)]
#[builder(pattern = "immutable", build_fn(private, name = "_build"), name = "BaseBuilder")]
#[non_exhaustive]
pub struct Base {
    /// Force block authoring
    #[builder(default)]
    #[serde(default, skip_serializing_if = "sdk_utils::is_default")]
    pub force_authoring: bool,
    /// Set node role
    #[builder(default)]
    #[serde(default, skip_serializing_if = "sdk_utils::is_default")]
    pub role: Role,
    /// Blocks pruning options
    #[builder(default)]
    #[serde(default, skip_serializing_if = "sdk_utils::is_default")]
    pub blocks_pruning: BlocksPruning,
    /// State pruning options
    #[builder(default)]
    #[serde(default, skip_serializing_if = "sdk_utils::is_default")]
    pub state_pruning: PruningMode,
    /// Set execution strategies
    #[builder(default)]
    #[serde(default, skip_serializing_if = "sdk_utils::is_default")]
    pub execution_strategy: ExecutionStrategy,
    /// Implementation name
    #[builder(default)]
    #[serde(default, skip_serializing_if = "sdk_utils::is_default")]
    pub impl_name: ImplName,
    /// Implementation version
    #[builder(default)]
    #[serde(default, skip_serializing_if = "sdk_utils::is_default")]
    pub impl_version: ImplVersion,
    /// Rpc settings
    #[builder(setter(into), default)]
    #[serde(default, skip_serializing_if = "sdk_utils::is_default")]
    pub rpc: Rpc,
    /// Network settings
    #[builder(setter(into), default)]
    #[serde(default, skip_serializing_if = "sdk_utils::is_default")]
    pub network: Network,
    /// Offchain worker settings
    #[builder(setter(into), default)]
    #[serde(default, skip_serializing_if = "sdk_utils::is_default")]
    pub offchain_worker: OffchainWorker,
    /// Enable color for substrate informant
    #[builder(default)]
    #[serde(default, skip_serializing_if = "sdk_utils::is_default")]
    pub informant_enable_color: bool,
    /// Additional telemetry endpoints
    #[builder(default)]
    #[serde(default, skip_serializing_if = "sdk_utils::is_default")]
    pub telemetry: Vec<(Multiaddr, u8)>,
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
                /// Enable color for substrate informant
                informant_enable_color: bool,
                /// Additional telemetry endpoints
                telemetry: Vec<(sdk_utils::Multiaddr, u8)>,
            });
        }
    }

impl Base {
    const NODE_NAME_MAX_LENGTH: usize = 64;

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
            informant_enable_color,
            telemetry,
        } = self;

        let base_path = BasePath::new(directory.as_ref());
        let config_dir = base_path.config_dir(chain_spec.id());

        let mut network = {
            let Network {
                listen_addresses,
                boot_nodes,
                force_synced,
                name,
                client_id,
                enable_mdns,
                allow_private_ip,
                allow_non_globals_in_dht,
            } = network;
            let name = name.unwrap_or_else(|| {
                names::Generator::with_naming(names::Name::Numbered)
                    .next()
                    .filter(|name| name.chars().count() < Self::NODE_NAME_MAX_LENGTH)
                    .expect("RNG is available on all supported platforms; qed")
            });

            let client_id = client_id.unwrap_or_else(|| format!("{impl_name}/v{impl_version}"));
            let config_dir = config_dir.join(DEFAULT_NETWORK_CONFIG_PATH);
            let listen_addresses = listen_addresses.into_iter().map(Into::into).collect::<Vec<_>>();

            NetworkConfiguration {
                listen_addresses,
                boot_nodes: chain_spec
                    .boot_nodes()
                    .iter()
                    .cloned()
                    .chain(boot_nodes.into_iter().map(Into::into))
                    .collect(),
                force_synced,
                transport: TransportConfig::Normal { enable_mdns, allow_private_ip },
                extra_sets: vec![NonDefaultSetConfig::new(
                    ProtocolName::Static("/subspace/cross-domain-messages"),
                    40,
                )],
                allow_non_globals_in_dht,
                ..NetworkConfiguration::new(
                    name,
                    client_id,
                    NodeKeyConfig::Ed25519(Secret::File(config_dir.join(NODE_KEY_ED25519_FILE))),
                    Some(config_dir),
                )
            }
        };

        // Increase default value of 25 to improve success rate of sync
        network.default_peers_set.out_peers = 50;
        // Full + Light clients
        network.default_peers_set.in_peers = 25 + 100;
        let keystore = KeystoreConfig::InMemory;

        // HACK: Tricky way to add extra endpoints as we can't push into telemetry
        // endpoints
        let telemetry_endpoints = match chain_spec.telemetry_endpoints() {
            Some(endpoints) => {
                let Ok(serde_json::Value::Array(extra_telemetry)) = serde_json::to_value(&telemetry) else {
                    unreachable!("Will always return an array")
                };
                let Ok(serde_json::Value::Array(telemetry)) = serde_json::to_value(endpoints) else {
                    unreachable!("Will always return an array")
                };

                serde_json::from_value(serde_json::Value::Array(
                    telemetry.into_iter().chain(extra_telemetry).collect::<Vec<_>>(),
                ))
                .expect("Serialization is always valid")
            }
            None => sc_service::config::TelemetryEndpoints::new(
                telemetry.into_iter().map(|(endpoint, n)| (endpoint.to_string(), n)).collect(),
            )
            .expect("Never returns an error"),
        };

        Configuration {
            impl_name,
            impl_version,
            tokio_handle: tokio::runtime::Handle::current(),
            transaction_pool: Default::default(),
            network,
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
            rpc_max_subs_per_conn,
            ws_max_out_buffer_capacity,
            prometheus_config: None,
            telemetry_endpoints: Some(telemetry_endpoints),
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
            informant_output_format: sc_informant::OutputFormat {
                enable_color: informant_enable_color,
            },
            runtime_cache_size: 2,
        }
    }
}

/// Node RPC builder
#[derive(Debug, Clone, Derivative, Builder, Deserialize, Serialize, PartialEq, Eq)]
#[derivative(Default)]
#[builder(pattern = "immutable", build_fn(private, name = "_build"), name = "RpcBuilder")]
#[non_exhaustive]
pub struct Rpc {
    /// RPC over HTTP binding address. `None` if disabled.
    #[builder(setter(strip_option), default)]
    #[serde(default, skip_serializing_if = "sdk_utils::is_default")]
    pub http: Option<SocketAddr>,
    /// RPC over Websockets binding address. `None` if disabled.
    #[builder(setter(strip_option), default)]
    #[serde(default, skip_serializing_if = "sdk_utils::is_default")]
    pub ws: Option<SocketAddr>,
    /// RPC over IPC binding path. `None` if disabled.
    #[builder(setter(strip_option), default)]
    #[serde(default, skip_serializing_if = "sdk_utils::is_default")]
    pub ipc: Option<String>,
    /// Maximum number of connections for WebSockets RPC server. `None` if
    /// default.
    #[builder(setter(strip_option), default)]
    #[serde(default, skip_serializing_if = "sdk_utils::is_default")]
    pub ws_max_connections: Option<usize>,
    /// CORS settings for HTTP & WS servers. `None` if all origins are
    /// allowed.
    #[builder(setter(strip_option), default)]
    #[serde(default, skip_serializing_if = "sdk_utils::is_default")]
    pub cors: Option<Vec<String>>,
    /// RPC methods to expose (by default only a safe subset or all of
    /// them).
    #[builder(default)]
    #[serde(default, skip_serializing_if = "sdk_utils::is_default")]
    pub methods: RpcMethods,
    /// Maximum payload of rpc request/responses.
    #[builder(setter(strip_option), default)]
    #[serde(default, skip_serializing_if = "sdk_utils::is_default")]
    pub max_payload: Option<usize>,
    /// Maximum payload of a rpc request
    #[builder(setter(strip_option), default)]
    #[serde(default, skip_serializing_if = "sdk_utils::is_default")]
    pub max_request_size: Option<usize>,
    /// Maximum payload of a rpc request
    #[builder(setter(strip_option), default)]
    #[serde(default, skip_serializing_if = "sdk_utils::is_default")]
    pub max_response_size: Option<usize>,
    /// Maximum allowed subscriptions per rpc connection
    #[builder(default)]
    #[serde(default, skip_serializing_if = "sdk_utils::is_default")]
    pub max_subs_per_conn: Option<usize>,
    /// Maximum size of the output buffer capacity for websocket
    /// connections.
    #[builder(setter(strip_option), default)]
    #[serde(default, skip_serializing_if = "sdk_utils::is_default")]
    pub ws_max_out_buffer_capacity: Option<usize>,
}

impl RpcBuilder {
    /// Dev configuration
    pub fn dev() -> Self {
        Self::default()
    }

    /// Gemini 3d configuration
    pub fn gemini_3d() -> Self {
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
    #[serde(default, skip_serializing_if = "sdk_utils::is_default")]
    pub enable_mdns: bool,
    /// Listen on some address for other nodes
    #[builder(default)]
    #[serde(default, skip_serializing_if = "sdk_utils::is_default")]
    pub allow_private_ip: bool,
    /// Allow non globals in network DHT
    #[builder(default)]
    #[serde(default, skip_serializing_if = "sdk_utils::is_default")]
    pub allow_non_globals_in_dht: bool,
    /// Listen on some address for other nodes
    #[builder(default)]
    #[serde(default, skip_serializing_if = "sdk_utils::is_default")]
    pub listen_addresses: Vec<Multiaddr>,
    /// Boot nodes
    #[builder(default)]
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub boot_nodes: Vec<MultiaddrWithPeerId>,
    /// Force node to think it is synced
    #[builder(default)]
    #[serde(default, skip_serializing_if = "sdk_utils::is_default")]
    pub force_synced: bool,
    /// Node name
    #[builder(setter(into, strip_option), default)]
    #[serde(default, skip_serializing_if = "sdk_utils::is_default")]
    pub name: Option<String>,
    /// Client id for telemetry (default is `{IMPL_NAME}/v{IMPL_VERSION}`)
    #[builder(setter(into, strip_option), default)]
    #[serde(default, skip_serializing_if = "sdk_utils::is_default")]
    pub client_id: Option<String>,
}

impl NetworkBuilder {
    /// Dev chain configuration
    pub fn dev() -> Self {
        Self::default().force_synced(true).allow_private_ip(true)
    }

    /// Gemini 3d configuration
    pub fn gemini_3d() -> Self {
        Self::default()
            .listen_addresses(vec![
                "/ip6/::/tcp/30333".parse().expect("hardcoded value is true"),
                "/ip4/0.0.0.0/tcp/30333".parse().expect("hardcoded value is true"),
            ])
            .enable_mdns(true)
    }

    /// Gemini 3d configuration
    pub fn devnet() -> Self {
        Self::default()
            .listen_addresses(vec![
                "/ip6/::/tcp/30333".parse().expect("hardcoded value is true"),
                "/ip4/0.0.0.0/tcp/30333".parse().expect("hardcoded value is true"),
            ])
            .enable_mdns(true)
    }
}

crate::generate_builder!(Base, Rpc, Network);

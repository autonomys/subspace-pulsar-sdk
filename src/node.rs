use futures::Stream;
use std::path::PathBuf;
use std::sync::{Arc, Weak};

use anyhow::Context;

use cirrus_runtime::GenesisConfig as ExecutionGenesisConfig;
use sc_chain_spec::ChainSpec;
use sc_executor::{WasmExecutionMethod, WasmtimeInstantiationStrategy};
use sc_network::config::{NodeKeyConfig, Secret};
use sc_service::config::{
    ExecutionStrategies, ExecutionStrategy, KeystoreConfig, NetworkConfiguration,
    OffchainWorkerConfig,
};
use sc_service::{
    BasePath, BlocksPruning, Configuration, DatabaseSource, PruningMode, Role, RpcMethods,
    TracingReceiver,
};
use sc_subspace_chain_specs::ConsensusChainSpec;
use subspace_fraud_proof::VerifyFraudProof;
use subspace_node::ExecutorDispatch;
use subspace_runtime::{GenesisConfig as ConsensusGenesisConfig, RuntimeApi};
use subspace_service::{FullClient, NewFull, SubspaceConfiguration};

use crate::Directory;

#[non_exhaustive]
#[derive(Debug, Default)]
pub enum Mode {
    #[default]
    Full,
}

#[non_exhaustive]
#[derive(Default)]
pub enum Chain {
    #[default]
    Gemini2a,
    Custom(ConsensusChainSpec<ConsensusGenesisConfig, ExecutionGenesisConfig>),
}

#[derive(Default)]
pub struct Builder {
    mode: Mode,
    chain: Chain,
    directory: Directory,
    name: Option<String>,
    port: u16,
    validate: bool,
}

impl Builder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn mode(mut self, ty: Mode) -> Self {
        self.mode = ty;
        self
    }

    pub fn chain(mut self, chain: Chain) -> Self {
        self.chain = chain;
        self
    }

    pub fn name(mut self, name: impl AsRef<str>) -> Self {
        if !name.as_ref().is_empty() {
            self.name = Some(name.as_ref().to_owned());
        }
        self
    }

    pub fn port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }

    pub fn at_directory(mut self, directory: impl Into<Directory>) -> Self {
        self.directory = directory.into();
        self
    }

    /// Start a node with supplied parameters
    pub async fn build(self) -> anyhow::Result<Node> {
        let Self {
            mode: Mode::Full,
            chain,
            directory,
            name,
            port: _,
            validate: _,
        } = self;

        let chain_spec = match chain {
            Chain::Gemini2a => ConsensusChainSpec::from_json_bytes(
                include_bytes!("../res/chain-spec-raw-gemini-2a.json").as_ref(),
            )
            .expect("Chain spec is always valid"),
            Chain::Custom(chain_spec) => chain_spec,
        };
        let base_directory = match directory {
            Directory::Custom(path) => path,
            _ => todo!("Handle other cases"),
        };
        let mut full_client =
            create_full_client(base_directory, chain_spec, name.unwrap_or_default()).await?;

        full_client.network_starter.start_network();

        let handle = tokio::spawn(async move {
            full_client.task_manager.future().await?;
            Ok(())
        });

        Ok(Node {
            _handle: Arc::new(handle),
            _weak: Arc::downgrade(&full_client.client),
        })
    }
}

// TODO: Allow customization of a bunch of these things
async fn create_full_client<CS: ChainSpec + 'static>(
    base_path: PathBuf,
    chain_spec: CS,
    node_name: String,
) -> anyhow::Result<
    NewFull<
        FullClient<RuntimeApi, ExecutorDispatch>,
        impl VerifyFraudProof + Clone + Send + Sync + 'static,
    >,
> {
    let config = create_configuration(
        BasePath::Permanenent(base_path),
        chain_spec,
        tokio::runtime::Handle::current(),
        node_name,
    );

    subspace_service::new_full::<RuntimeApi, ExecutorDispatch>(
        config,
        true,
        sc_consensus_slots::SlotProportion::new(2f32 / 3f32),
    )
    .await
    .context("Failed to build a full subspace node")
}

fn create_configuration<CS: ChainSpec + 'static>(
    base_path: BasePath,
    chain_spec: CS,
    tokio_handle: tokio::runtime::Handle,
    node_name: String,
) -> SubspaceConfiguration {
    const DEFAULT_NETWORK_CONFIG_PATH: &str = "default-network-config-path";
    const NODE_KEY_ED25519_FILE: &str = "node_key_ed25519_file";

    let impl_name = "Subspace-sdk".to_owned();
    let impl_version = "sc_cli_impl_version".to_string(); // env!("SUBSTRATE_CLI_IMPL_VERSION")
    let config_dir = base_path.config_dir(chain_spec.id());
    let net_config_dir = config_dir.join(DEFAULT_NETWORK_CONFIG_PATH);
    let client_id = format!("{}/v{}", impl_name, impl_version);
    let mut network = NetworkConfiguration {
        listen_addresses: vec![
            "/ip6/::/tcp/30333".parse().expect("Multiaddr is correct"),
            "/ip4/0.0.0.0/tcp/30333"
                .parse()
                .expect("Multiaddr is correct"),
        ],
        boot_nodes: chain_spec.boot_nodes().to_vec(),
        ..NetworkConfiguration::new(
            node_name,
            client_id,
            NodeKeyConfig::Ed25519(Secret::File(net_config_dir.join(NODE_KEY_ED25519_FILE))),
            Some(net_config_dir),
        )
    };

    // Increase default value of 25 to improve success rate of sync
    network.default_peers_set.out_peers = 50;
    // Full + Light clients
    network.default_peers_set.in_peers = 25 + 100;
    let role = Role::Authority;
    let (keystore_remote, keystore) = (None, KeystoreConfig::InMemory);
    let telemetry_endpoints = chain_spec.telemetry_endpoints().clone();

    // Default value are used for many of parameters
    SubspaceConfiguration {
        base: Configuration {
            impl_name,
            impl_version,
            tokio_handle,
            transaction_pool: Default::default(),
            network,
            keystore_remote,
            keystore,
            database: DatabaseSource::ParityDb {
                path: config_dir.join("paritydb").join("full"),
            },
            state_cache_size: 67_108_864,
            state_cache_child_ratio: None,
            // TODO: Change to constrained eventually (need DSN for this)
            state_pruning: Some(PruningMode::blocks_pruning(1024)),
            blocks_pruning: BlocksPruning::Some(1024),
            wasm_method: WasmExecutionMethod::Compiled {
                instantiation_strategy: WasmtimeInstantiationStrategy::PoolingCopyOnWrite,
            },
            wasm_runtime_overrides: None,
            execution_strategies: ExecutionStrategies {
                syncing: ExecutionStrategy::AlwaysWasm,
                importing: ExecutionStrategy::AlwaysWasm,
                block_construction: ExecutionStrategy::AlwaysWasm,
                offchain_worker: ExecutionStrategy::AlwaysWasm,
                other: ExecutionStrategy::AlwaysWasm,
            },
            rpc_http: None,
            rpc_ws: Some("127.0.0.1:9947".parse().expect("IP and port are valid")),
            rpc_ipc: None,
            // necessary in order to use `peers` method to show number of node peers during sync
            rpc_methods: RpcMethods::Unsafe,
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
            force_authoring: std::env::var("FORCE_AUTHORING")
                .map(|force_authoring| force_authoring.as_str() == "1")
                .unwrap_or_default(),
            disable_grandpa: false,
            dev_key_seed: None,
            tracing_targets: None,
            tracing_receiver: TracingReceiver::Log,
            chain_spec: Box::new(chain_spec),
            max_runtime_instances: 8,
            announce_block: true,
            role,
            base_path: Some(base_path),
            informant_output_format: Default::default(),
            runtime_cache_size: 2,
            rpc_max_subs_per_conn: None,
        },
        force_new_slot_notifications: false,
        dsn_config: None,
    }
}

#[derive(Clone)]
pub struct Node {
    _handle: Arc<tokio::task::JoinHandle<anyhow::Result<()>>>,
    _weak: Weak<FullClient<RuntimeApi, ExecutorDispatch>>,
}

#[non_exhaustive]
pub struct Info {
    pub version: String,
    pub chain: Chain,
    pub mode: Mode,
    pub name: Option<String>,
    pub connected_peers: u64,
    pub best_block: u64,
    pub total_space_pledged: u64,
    pub total_history_size: u64,
    pub space_pledged: u64,
}

#[derive(Debug)]
pub struct Block {
    _ensure_cant_construct: (),
}

pub struct BlockStream {
    _ensure_cant_construct: (),
}

impl Stream for BlockStream {
    type Item = Block;
    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        todo!()
    }
}

impl Node {
    pub fn builder() -> Builder {
        Builder::new()
    }

    pub async fn sync(&mut self) {}

    // Leaves the network and gracefully shuts down
    pub async fn close(self) {}

    // Runs `.close()` and also wipes node's state
    pub async fn wipe(self) {}

    pub async fn get_info(&mut self) -> Info {
        todo!()
    }

    pub async fn subscribe_new_blocks(&mut self) -> BlockStream {
        todo!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn test_start_node() {
        let dir = tempdir::TempDir::new("ab").unwrap();
        Node::builder()
            .at_directory(dir.as_ref())
            .build()
            .await
            .unwrap();
    }
}

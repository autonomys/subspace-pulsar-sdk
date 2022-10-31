use futures::Stream;
use std::sync::{Arc, Weak};

use anyhow::Context;

use sc_chain_spec::ChainSpec;
use sc_executor::{WasmExecutionMethod, WasmtimeInstantiationStrategy};
use sc_network::config::{NodeKeyConfig, Secret};
use sc_service::config::{
    ExecutionStrategies, ExecutionStrategy, KeystoreConfig, NetworkConfiguration,
    OffchainWorkerConfig,
};
use sc_service::{
    BasePath, BlocksPruning, Configuration, DatabaseSource, PruningMode, RpcMethods,
    TracingReceiver,
};
use sc_subspace_chain_specs::ConsensusChainSpec;
use subspace_node::ExecutorDispatch;
use subspace_runtime::{GenesisConfig as ConsensusGenesisConfig, RuntimeApi};
use subspace_service::{FullClient, SubspaceConfiguration};
use system_domain_runtime::GenesisConfig as ExecutionGenesisConfig;

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
    Custom(Box<ConsensusChainSpec<ConsensusGenesisConfig, ExecutionGenesisConfig>>),
}

impl std::fmt::Debug for Chain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Gemini2a => write!(f, "Gemini2a"),
            Self::Custom(custom) => write!(f, "Custom({})", custom.name()),
        }
    }
}

struct Role(sc_service::Role);

impl Default for Role {
    fn default() -> Self {
        Self(sc_service::Role::Full)
    }
}

#[derive(Default)]
pub struct Builder {
    mode: Mode,
    chain: Chain,
    directory: Directory,
    name: Option<String>,
    force_authoring: bool,
    role: Role,
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

    pub fn at_directory(mut self, directory: impl Into<Directory>) -> Self {
        self.directory = directory.into();
        self
    }

    pub fn force_authoring(mut self, force_authoring: bool) -> Self {
        self.force_authoring = force_authoring;
        self
    }

    pub fn role(mut self, role: sc_service::Role) -> Self {
        self.role = Role(role);
        self
    }

    /// Start a node with supplied parameters
    pub async fn build(self) -> anyhow::Result<Node> {
        let Self {
            mode: Mode::Full,
            chain,
            directory,
            name,
            force_authoring,
            role: Role(role),
        } = self;

        let chain_spec = match chain {
            Chain::Gemini2a => {
                subspace_node::chain_spec::gemini_2a().expect("Gemini-2a spec should be compiled")
            }
            Chain::Custom(chain_spec) => *chain_spec,
        };
        let base_path = match directory {
            Directory::Custom(path) => BasePath::Permanenent(path),
            Directory::Tmp => {
                BasePath::new_temp_dir().context("Failed to create temporary directory")?
            }
            Directory::Default => BasePath::Permanenent(
                dirs::data_local_dir()
                    .expect("Can't find local data directory, needs to be specified explicitly")
                    .join("subspace-node"),
            ),
        };

        let mut full_client = subspace_service::new_full::<RuntimeApi, ExecutorDispatch>(
            create_configuration(base_path, chain_spec, name, force_authoring, role).await,
            true,
            sc_consensus_slots::SlotProportion::new(2f32 / 3f32),
        )
        .await
        .context("Failed to build a full subspace node")?;

        let rpc_handle = full_client.rpc_handlers.handle();

        full_client.network_starter.start_network();

        let handle = tokio::spawn(async move {
            full_client.task_manager.future().await?;
            Ok(())
        });

        Ok(Node {
            _handle: Arc::new(handle),
            _weak: Arc::downgrade(&full_client.client),
            rpc_handle,
        })
    }
}

async fn create_configuration<CS: ChainSpec + 'static>(
    base_path: BasePath,
    chain_spec: CS,
    node_name: Option<String>,
    force_authoring: bool,
    role: sc_service::Role,
) -> SubspaceConfiguration {
    const NODE_KEY_ED25519_FILE: &str = "secret_ed25519";
    const DEFAULT_NETWORK_CONFIG_PATH: &str = "network";

    let impl_name = env!("CARGO_PKG_NAME").to_owned();
    let impl_version = env!("CARGO_PKG_VERSION").to_string(); // TODO: include git revision here
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
            node_name.unwrap_or_default(),
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
    SubspaceConfiguration {
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
            state_cache_size: 67_108_864,
            state_cache_child_ratio: None,
            // TODO: Change to constrained eventually (need DSN for this)
            state_pruning: Some(PruningMode::ArchiveAll),
            blocks_pruning: BlocksPruning::All,
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
            force_authoring,
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
    rpc_handle: Arc<jsonrpsee_core::server::rpc_module::RpcModule<()>>,
}

#[derive(Debug)]
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

mod farmer_rpc_client {
    use super::*;

    use futures::{FutureExt, Stream};
    use jsonrpsee_core::server::rpc_module::Subscription;
    use std::pin::Pin;

    use subspace_archiving::archiver::ArchivedSegment;
    use subspace_core_primitives::{Piece, PieceIndex, RecordsRoot, SegmentIndex};
    use subspace_farmer::rpc_client::{Error, RpcClient};
    use subspace_rpc_primitives::{
        FarmerProtocolInfo, RewardSignatureResponse, RewardSigningInfo, SlotInfo, SolutionResponse,
    };

    fn subscription_to_stream<T: serde::de::DeserializeOwned>(
        mut subscription: Subscription,
    ) -> impl Stream<Item = T> {
        futures::stream::poll_fn(move |cx| {
            Box::pin(subscription.next())
                .poll_unpin(cx)
                .map(|x| x.map(Result::ok).flatten().map(|(x, _)| x))
        })
    }

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

    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn test_start_node() {
        Node::builder()
            .at_directory(Directory::Tmp)
            .build()
            .await
            .unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_rpc() {
        let node = Node::builder()
            .at_directory(Directory::Tmp)
            .chain(Chain::Custom(Box::new(
                subspace_node::chain_spec::dev_config().unwrap(),
            )))
            .build()
            .await
            .unwrap();

        assert!(node.farmer_protocol_info().await.is_ok());
    }
}

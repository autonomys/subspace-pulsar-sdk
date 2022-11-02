use futures::channel::{mpsc, oneshot};
use futures::{FutureExt, SinkExt, Stream, StreamExt};
use std::io;
use std::path::Path;
use std::sync::{Arc, Weak};

use anyhow::Context;

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
use subspace_runtime::{GenesisConfig as ConsensusGenesisConfig, RuntimeApi};
use subspace_service::{FullClient, SubspaceConfiguration};
use system_domain_runtime::GenesisConfig as ExecutionGenesisConfig;

pub mod chain_spec;

#[non_exhaustive]
#[derive(Debug, Default)]
pub enum Mode {
    #[default]
    Full,
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

    pub fn name(mut self, name: impl AsRef<str>) -> Self {
        if !name.as_ref().is_empty() {
            self.name = Some(name.as_ref().to_owned());
        }
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
    pub async fn build(
        self,
        directory: impl AsRef<Path>,
        chain_spec: ConsensusChainSpec<ConsensusGenesisConfig, ExecutionGenesisConfig>,
    ) -> anyhow::Result<Node> {
        const NODE_KEY_ED25519_FILE: &str = "secret_ed25519";
        const DEFAULT_NETWORK_CONFIG_PATH: &str = "network";

        let Self {
            mode: Mode::Full,
            name,
            force_authoring,
            role: Role(role),
        } = self;

        let base_path = BasePath::new(directory);
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
                name.unwrap_or_default(),
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
        };

        let slot_proportion = sc_consensus_slots::SlotProportion::new(2f32 / 3f32);
        let mut full_client = subspace_service::new_full::<RuntimeApi, ExecutorDispatch>(
            configuration,
            true,
            slot_proportion,
        )
        .await
        .context("Failed to build a full subspace node")?;

        let rpc_handle = full_client.rpc_handlers.handle();
        full_client.network_starter.start_network();
        let (stop_sender, mut stop_receiver) = mpsc::channel::<oneshot::Sender<()>>(1);

        tokio::spawn(async move {
            let stop_sender = futures::select! {
                opt_sender = stop_receiver.next() => {
                    match opt_sender {
                        Some(sender) => sender,
                        None => return,
                    }
                }
                result = full_client.task_manager.future().fuse() => {
                    let _ = result;
                    return;
                }
            };
            drop(full_client.task_manager);
            let _ = stop_sender.send(());
        });

        Ok(Node {
            _weak: Arc::downgrade(&full_client.client),
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

#[derive(Clone)]
pub struct Node {
    _weak: Weak<FullClient<RuntimeApi, ExecutorDispatch>>,
    rpc_handle: Arc<jsonrpsee_core::server::rpc_module::RpcModule<()>>,
    stop_sender: mpsc::Sender<oneshot::Sender<()>>,
}

#[derive(Debug)]
#[non_exhaustive]
pub struct ChainInfo {
    _ensure_cant_construct: (),
}

#[derive(Debug)]
#[non_exhaustive]
pub struct Info {
    pub version: String,
    pub chain: ChainInfo,
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
    pub async fn close(mut self) {
        let (stop_sender, stop_receiver) = oneshot::channel();
        drop(self.stop_sender.send(stop_sender).await);
        let _ = stop_receiver.await;
    }

    // Runs `.close()` and also wipes node's state
    pub async fn wipe(path: impl AsRef<Path>) -> io::Result<()> {
        tokio::fs::remove_dir_all(path).await
    }

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
                .map(|x| x.and_then(Result::ok).map(|(x, _)| x))
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
            .build(dir.path(), chain_spec::dev_config().unwrap())
            .await
            .unwrap();
        let plot_dir = TempDir::new("test").unwrap();
        let plots = [PlotDescription::new(
            plot_dir.as_ref(),
            bytesize::ByteSize::mb(10),
        )];
        let farmer = Farmer::builder()
            .build(Default::default(), node.clone(), &plots)
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;

        farmer.close().await;
        node.close().await;
    }
}

use std::{collections::HashMap, io, num::NonZeroUsize, path::PathBuf, sync::Arc};

use anyhow::Context;
use bytesize::ByteSize;
use futures::{prelude::*, stream::FuturesUnordered};
use serde::{Deserialize, Serialize};
use subspace_core_primitives::{PieceIndexHash, SectorIndex, PLOT_SECTOR_SIZE};
use subspace_farmer::single_disk_plot::{
    piece_reader::PieceReader, SingleDiskPlot, SingleDiskPlotError, SingleDiskPlotId,
    SingleDiskPlotInfo, SingleDiskPlotOptions, SingleDiskPlotSummary,
};
use subspace_farmer_components::plotting::PlottedSector;
use subspace_networking::libp2p::identity::Keypair;
use subspace_networking::{
    CustomRecordStore, LimitedSizeRecordStorageWrapper, MemoryProviderStorage, Node as DSNNode,
    NodeRunner as DSNNodeRunner, ParityDbRecordStorage,
};
use subspace_rpc_primitives::SolutionResponse;
use tokio::sync::{oneshot, watch, Mutex};

use crate::{Node, PublicKey};

pub use builder::{Builder, Config, Dsn, DsnBuilder};

/// Description of the cache
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[non_exhaustive]
pub struct CacheDescription {
    /// Path to the cache description
    pub directory: PathBuf,
    /// Space which you want to dedicate
    #[serde(with = "bytesize_serde")]
    pub space_dedicated: ByteSize,
}

impl CacheDescription {
    fn record_store(
        self,
        keypair: &Keypair,
    ) -> parity_db::Result<
        subspace_networking::CustomRecordStore<
            LimitedSizeRecordStorageWrapper<ParityDbRecordStorage>,
            MemoryProviderStorage,
        >,
    > {
        ParityDbRecordStorage::new(&self.directory).map(|storage| {
            CustomRecordStore::new(
                LimitedSizeRecordStorageWrapper::new(
                    storage,
                    NonZeroUsize::new(self.space_dedicated.as_u64() as _)
                        .expect("Always more than 1Mib"),
                    subspace_networking::peer_id(keypair),
                ),
                MemoryProviderStorage::default(),
            )
        })
    }
}

const MIN_CACHE_SIZE: ByteSize = ByteSize::mib(1);

/// Error type for cache description constructor
#[derive(Debug, Clone, Copy, thiserror::Error)]
#[error("Cache should be larger than {MIN_CACHE_SIZE}")]
pub struct CacheTooSmall;

impl CacheDescription {
    /// Construct Plot description
    pub fn new(
        directory: impl Into<PathBuf>,
        space_dedicated: ByteSize,
    ) -> Result<Self, CacheTooSmall> {
        if space_dedicated < MIN_CACHE_SIZE {
            return Err(CacheTooSmall);
        }
        Ok(Self {
            directory: directory.into(),
            space_dedicated,
        })
    }

    /// Wipe all the data from the plot
    pub async fn wipe(self) -> io::Result<()> {
        tokio::fs::remove_dir_all(self.directory).await
    }
}

/// Description of the plot
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[non_exhaustive]
pub struct PlotDescription {
    /// Path of the plot
    pub directory: PathBuf,
    /// Space which you want to pledge
    #[serde(with = "bytesize_serde")]
    pub space_pledged: ByteSize,
}

impl PlotDescription {
    /// Construct Plot description
    pub fn new(directory: impl Into<PathBuf>, space_pledged: ByteSize) -> Result<Self, BuildError> {
        const SECTOR_SIZE: ByteSize = ByteSize::b(PLOT_SECTOR_SIZE);

        if space_pledged <= SECTOR_SIZE {
            Ok(Self {
                directory: directory.into(),
                space_pledged,
            })
        } else {
            Err(BuildError::PlotTooSmall(space_pledged, SECTOR_SIZE))
        }
    }

    /// Wipe all the data from the plot
    pub async fn wipe(self) -> io::Result<()> {
        tokio::fs::remove_dir_all(self.directory).await
    }
}

mod builder {
    use crate::generate_builder;
    use derive_builder::Builder;
    use libp2p_core::Multiaddr;
    use serde::{Deserialize, Serialize};

    fn default_piece_receiver_batch_size() -> usize {
        12
    }

    fn default_piece_publisher_batch_size() -> usize {
        12
    }

    /// Technical type which stores all
    #[derive(Debug, Clone, Default, Builder, Serialize, Deserialize)]
    #[builder(pattern = "immutable", build_fn(name = "_build"), name = "Builder")]
    #[non_exhaustive]
    pub struct Config {
        /// Determines whether we allow keeping non-global (private, shared, loopback..) addresses in Kademlia DHT.
        #[builder(default = "default_piece_receiver_batch_size()")]
        #[serde(default = "default_piece_receiver_batch_size")]
        pub piece_receiver_batch_size: usize,
        /// Determines whether we allow keeping non-global (private, shared, loopback..) addresses in Kademlia DHT.
        #[builder(default = "default_piece_publisher_batch_size()")]
        #[serde(default = "default_piece_publisher_batch_size")]
        pub piece_publisher_batch_size: usize,
        /// DSN options
        #[builder(default, setter(into))]
        pub dsn: Dsn,
    }

    /// Farmer DSN
    #[derive(Debug, Clone, Default, Builder, Serialize, Deserialize)]
    #[builder(pattern = "owned", build_fn(name = "_build"), name = "DsnBuilder")]
    #[non_exhaustive]
    pub struct Dsn {
        /// Listen on
        #[builder(default)]
        pub listen_on: Vec<Multiaddr>,
        /// Bootstrap nodes
        #[builder(default)]
        pub bootstrap_nodes: Vec<Multiaddr>,
        /// Determines whether we allow keeping non-global (private, shared, loopback..) addresses in Kademlia DHT.
        #[builder(default)]
        pub allow_non_global_addresses_in_dht: bool,
    }

    generate_builder!(Dsn);
}

impl builder::Dsn {
    async fn configure_dsn(
        self,
        cache: CacheDescription,
    ) -> Result<(DSNNode, DSNNodeRunner<ConfiguredRecordStore>), BuildError> {
        use subspace_networking::{
            BootstrappedNetworkingParameters, Config, PieceByHashRequestHandler,
            PieceByHashResponse, PieceKey,
        };

        let Self {
            listen_on,
            bootstrap_nodes,
            allow_non_global_addresses_in_dht,
        } = self;

        let readers_and_pieces = Arc::new(std::sync::Mutex::new(None::<ReadersAndPieces>));
        let weak_readers_and_pieces = Arc::downgrade(&readers_and_pieces);

        let handle = tokio::runtime::Handle::current();
        let default_config = Config::with_generated_keypair();

        let config = Config::<ConfiguredRecordStore> {
            listen_on: listen_on
                .into_iter()
                .map(|a| {
                    a.to_string()
                        .parse()
                        .expect("Convertion between 2 libp2p version. Never panics")
                })
                .collect::<Vec<_>>(),
            networking_parameters_registry: BootstrappedNetworkingParameters::new(
                bootstrap_nodes
                    .into_iter()
                    .map(|a| {
                        a.to_string()
                            .parse()
                            .expect("Convertion between 2 libp2p version. Never panics")
                    })
                    .collect::<Vec<_>>(),
            )
            .boxed(),
            request_response_protocols: vec![PieceByHashRequestHandler::create(move |req| {
                let PieceKey::Sector(piece_index_hash) = req.key else { return None };
                let Some(readers_and_pieces) = weak_readers_and_pieces.upgrade() else { return None };
                let readers_and_pieces = readers_and_pieces.lock().unwrap();
                let Some(readers_and_pieces) = readers_and_pieces.as_ref() else { return None };
                let Some(piece_details) = readers_and_pieces.pieces.get(&piece_index_hash).copied() else { return None };
                let mut reader = readers_and_pieces
                    .readers
                    .get(piece_details.plot_offset)
                    .cloned()
                    .expect("Offsets strictly correspond to existing plots; qed");

                let handle = handle.clone();
                let result = tokio::task::block_in_place(move || {
                    handle.block_on(
                        reader.read_piece(piece_details.sector_index, piece_details.piece_offset),
                    )
                });

                Some(PieceByHashResponse { piece: result })
            })],
            record_store: cache.record_store(&default_config.keypair)?,
            allow_non_global_addresses_in_dht,
            ..default_config
        };

        subspace_networking::create(config)
            .await
            .map_err(Into::into)
    }
}

/// Build Error
#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    /// Failed to create single disk plot
    #[error("Single disk plot creation error: {0}")]
    SingleDiskPlotCreate(#[from] SingleDiskPlotError),
    /// No plots were supplied during building
    #[error("Supply at least one plot")]
    NoPlotsSupplied,
    /// Failed to connect to DSN
    #[error("Failed to connect to DSN: {0}")]
    DSNCreate(#[from] subspace_networking::CreationError),
    /// Failed to fetch data from the node
    #[error("Failed to fetch data from node: {0}")]
    RPCError(#[source] subspace_farmer::RpcClientError),
    /// Failed to create parity db record storage
    #[error("Failed to create parity db record storage: {0}")]
    ParityDbError(#[from] parity_db::Error),
    /// Plot was too small
    #[error("Plot size was too small {0} (should be at least {1})")]
    PlotTooSmall(ByteSize, ByteSize),
}

// Type alias for currently configured Kademlia's custom record store.
type ConfiguredRecordStore = subspace_networking::CustomRecordStore<
    subspace_networking::LimitedSizeRecordStorageWrapper<
        subspace_networking::ParityDbRecordStorage,
    >,
    subspace_networking::MemoryProviderStorage,
>;

#[derive(Debug, Copy, Clone)]
struct PieceDetails {
    plot_offset: usize,
    sector_index: SectorIndex,
    piece_offset: u64,
}

#[derive(Debug)]
struct ReadersAndPieces {
    readers: Vec<PieceReader>,
    pieces: HashMap<PieceIndexHash, PieceDetails>,
}

impl Builder {
    /// Get configuration for saving on disk
    pub fn configuration(&self) -> Config {
        self._build().expect("Build is infallible")
    }

    /// Open and start farmer
    pub async fn build(
        self,
        reward_address: PublicKey,
        node: Node,
        plots: &[PlotDescription],
        cache: CacheDescription,
    ) -> Result<Farmer, BuildError> {
        self.configuration()
            .build(reward_address, node, plots, cache)
            .await
    }
}

impl Config {
    /// Open and start farmer
    pub async fn build(
        self,
        reward_address: PublicKey,
        node: Node,
        plots: &[PlotDescription],
        cache: CacheDescription,
    ) -> Result<Farmer, BuildError> {
        let Self {
            mut dsn,
            piece_receiver_batch_size,
            piece_publisher_batch_size,
        } = self;

        if plots.is_empty() {
            return Err(BuildError::NoPlotsSupplied);
        }

        if dsn.bootstrap_nodes.is_empty() {
            use subspace_farmer::RpcClient;

            dsn.bootstrap_nodes = node
                .farmer_app_info()
                .await
                .map_err(BuildError::RPCError)?
                .dsn_bootstrap_nodes
                .into_iter()
                .map(|a| {
                    a.to_string()
                        .parse()
                        .expect("Convertion between 2 libp2p version. Never panics")
                })
                .collect::<Vec<_>>();
        }

        let mut single_disk_plots = Vec::with_capacity(plots.len());
        let mut plot_info = HashMap::with_capacity(plots.len());
        let (dsn_node, mut dsn_node_runner) = dsn.configure_dsn(cache).await?;

        for description in plots {
            let directory = description.directory.clone();
            let allocated_space = description.space_pledged.as_u64();
            let description = SingleDiskPlotOptions {
                allocated_space,
                directory: directory.clone(),
                reward_address: *reward_address,
                rpc_client: node.clone(),
                dsn_node: dsn_node.clone(),
                piece_receiver_batch_size,
                piece_publisher_batch_size,
            };
            let single_disk_plot = SingleDiskPlot::new(description)?;

            let mut handlers = Vec::new();
            let progress = {
                let (sender, receiver) = watch::channel::<Option<_>>(None);
                let handler = single_disk_plot.on_sector_plotted(Arc::new(move |sector| {
                    let _ = sender.send(Some(sector.clone()));
                }));
                handlers.push(handler);
                receiver
            };
            let solutions = {
                let (sender, receiver) = watch::channel::<Option<_>>(None);
                let handler = single_disk_plot.on_solution(Arc::new(move |solution| {
                    let _ = sender.send(Some(solution.clone()));
                }));
                handlers.push(handler);
                receiver
            };
            let plot = Plot {
                directory: directory.clone(),
                allocated_space,
                progress,
                solutions,
                initial_plotting_progress: Arc::new(Mutex::new(InitialPlottingProgress {
                    starting_sector: single_disk_plot.plotted_sectors_count(),
                    current_sector: single_disk_plot.plotted_sectors_count(),
                    total_sectors: allocated_space / subspace_core_primitives::PLOT_SECTOR_SIZE,
                })),
                _handlers: handlers,
            };
            plot_info.insert(directory, plot);
            single_disk_plots.push(single_disk_plot);
        }

        let mut single_disk_plots_stream = single_disk_plots
            .into_iter()
            .map(SingleDiskPlot::run)
            .collect::<FuturesUnordered<_>>();

        tokio::spawn(async move {
            dsn_node_runner.run().await;
        });

        let (cmd_sender, cmd_receiver) = oneshot::channel::<()>();

        let handle = tokio::task::spawn_blocking({
            let handle = tokio::runtime::Handle::current();
            let node = node.clone();
            move || {
                let result_maybe_sender = handle.block_on(futures::future::select(
                    single_disk_plots_stream.next(),
                    cmd_receiver,
                ));
                match result_maybe_sender {
                    // If node is closed when we might get some random error, so just ignore it
                    future::Either::Left((_, _)) if handle.block_on(node.is_closed()) => Ok(()),
                    future::Either::Left((maybe_result, _)) => {
                        maybe_result.expect("There is at least one plot")
                    }
                    future::Either::Right((_, _)) => Ok(()),
                }
            }
        });

        Ok(Farmer {
            cmd_sender: Arc::new(Mutex::new(Some(cmd_sender))),
            reward_address,
            plot_info: Arc::new(plot_info),
            _node: node,
            handle: Arc::new(Mutex::new(handle)),
        })
    }
}

/// Farmer structure
#[derive(Clone, Debug)]
pub struct Farmer {
    cmd_sender: Arc<Mutex<Option<oneshot::Sender<()>>>>,
    reward_address: PublicKey,
    plot_info: Arc<HashMap<PathBuf, Plot>>,
    _node: Node,
    handle: Arc<Mutex<tokio::task::JoinHandle<anyhow::Result<()>>>>,
}

/// Info about some plot
#[derive(Debug)]
#[non_exhaustive]
// TODO: Should it be versioned?
pub struct PlotInfo {
    /// ID of the plot
    pub id: SingleDiskPlotId,
    /// Genesis hash of the chain used for plot creation
    pub genesis_hash: [u8; 32],
    /// Public key of identity used for plot creation
    pub public_key: PublicKey,
    /// First sector index in this plot
    ///
    /// Multiple plots can reuse the same identity, but they have to use different ranges for
    /// sector indexes or else they'll essentially plot the same data and will not result in
    /// increased probability of winning the reward.
    pub first_sector_index: SectorIndex,
    /// How much space in bytes is allocated for this plot
    pub allocated_space: ByteSize,
}

impl From<SingleDiskPlotInfo> for PlotInfo {
    fn from(info: SingleDiskPlotInfo) -> Self {
        let SingleDiskPlotInfo::V0 {
            id,
            genesis_hash,
            public_key,
            first_sector_index,
            allocated_space,
        } = info;
        Self {
            id,
            genesis_hash,
            public_key: super::PublicKey(public_key),
            first_sector_index,
            allocated_space: ByteSize::b(allocated_space),
        }
    }
}

/// Farmer info
#[derive(Debug)]
#[non_exhaustive]
pub struct Info {
    /// Version of the farmer
    pub version: String,
    /// Reward address of our farmer
    pub reward_address: PublicKey,
    // TODO: add dsn peers info
    // pub dsn_peers: u64,
    /// Info about each plot
    pub plots_info: HashMap<PathBuf, PlotInfo>,
    /// Sector size in bits
    pub sector_size: u64,
}

/// Initial plotting progress
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InitialPlottingProgress {
    /// Number of sectors from which we started plotting
    pub starting_sector: u64,
    /// Current number of sectors
    pub current_sector: u64,
    /// Total number of sectors on disk
    pub total_sectors: u64,
}

/// Plot structure
#[derive(Debug)]
pub struct Plot {
    directory: PathBuf,
    progress: watch::Receiver<Option<PlottedSector>>,
    solutions: watch::Receiver<Option<SolutionResponse>>,
    initial_plotting_progress: Arc<Mutex<InitialPlottingProgress>>,
    allocated_space: u64,
    _handlers: Vec<event_listener_primitives::HandlerId>,
}

#[pin_project::pin_project]
struct InitialPlottingProgressStreamInner<S> {
    last_initial_plotting_progress: InitialPlottingProgress,
    #[pin]
    stream: S,
}

impl<S: Stream> Stream for InitialPlottingProgressStreamInner<S>
where
    S: Stream<Item = InitialPlottingProgress>,
{
    type Item = InitialPlottingProgress;

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        let this = self.project();
        match this.stream.poll_next(cx) {
            result @ std::task::Poll::Ready(Some(progress)) => {
                *this.last_initial_plotting_progress = progress;
                result
            }
            result => result,
        }
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        let left = self.last_initial_plotting_progress.total_sectors
            - self.last_initial_plotting_progress.current_sector;
        (left as usize, Some(left as usize))
    }
}

/// Initial plotting progress stream
#[pin_project::pin_project]
pub struct InitialPlottingProgressStream {
    #[pin]
    boxed_stream:
        std::pin::Pin<Box<dyn Stream<Item = InitialPlottingProgress> + Send + Sync + Unpin>>,
}

impl Stream for InitialPlottingProgressStream {
    type Item = InitialPlottingProgress;
    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.project().boxed_stream.poll_next(cx)
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.boxed_stream.size_hint()
    }
}

impl Plot {
    /// Plot location
    pub fn directory(&self) -> &PathBuf {
        &self.directory
    }

    /// Plot size
    pub fn allocated_space(&self) -> ByteSize {
        ByteSize::b(self.allocated_space)
    }

    /// Will return a stream of initial plotting progress which will end once we finish plotting
    pub async fn subscribe_initial_plotting_progress(&self) -> InitialPlottingProgressStream {
        let initial = *self.initial_plotting_progress.lock().await;
        if initial.current_sector == initial.total_sectors {
            return InitialPlottingProgressStream {
                boxed_stream: Box::pin(futures::stream::iter(None)),
            };
        }

        let stream = tokio_stream::wrappers::WatchStream::new(self.progress.clone())
            .filter_map({
                let initial_plotting_progress = Arc::clone(&self.initial_plotting_progress);
                move |_| {
                    let initial_plotting_progress = Arc::clone(&initial_plotting_progress);
                    async move {
                        let mut guard = initial_plotting_progress.lock().await;
                        guard.current_sector += 1;
                        Some(*guard)
                    }
                }
            })
            .take_while(
                |InitialPlottingProgress {
                     current_sector,
                     total_sectors,
                     ..
                 }| futures::future::ready(current_sector != total_sectors),
            )
            .chain(futures::stream::once({
                let mut initial_progress = *self.initial_plotting_progress.lock().await;
                initial_progress.current_sector = initial_progress.total_sectors;
                futures::future::ready(initial_progress)
            }));
        let last_initial_plotting_progress = *self.initial_plotting_progress.lock().await;

        InitialPlottingProgressStream {
            boxed_stream: Box::pin(Box::pin(InitialPlottingProgressStreamInner {
                stream,
                last_initial_plotting_progress,
            })),
        }
    }

    /// New solution subscription
    pub async fn subscribe_new_solutions(
        &self,
    ) -> impl Stream<Item = SolutionResponse> + Send + Sync + Unpin {
        tokio_stream::wrappers::WatchStream::new(self.solutions.clone())
            .filter_map(futures::future::ready)
    }
}

impl Farmer {
    /// Farmer builder
    pub fn builder() -> Builder {
        Builder::default()
    }

    /// Gets plot info
    pub async fn get_info(&self) -> anyhow::Result<Info> {
        let plots_info = tokio::task::spawn_blocking({
            let dirs = self.plot_info.keys().cloned().collect::<Vec<_>>();
            || {
                dirs.into_iter()
                    .map(SingleDiskPlot::collect_summary)
                    .collect::<Vec<_>>()
            }
        })
        .await?
        .into_iter()
        .map(|summary| match summary {
            SingleDiskPlotSummary::Found { info, directory } => Ok((directory, info.into())),
            SingleDiskPlotSummary::NotFound { directory } => {
                Err(anyhow::anyhow!("Didn't found plot at `{directory:?}'"))
            }
            SingleDiskPlotSummary::Error { directory, error } => {
                Err(error).context(format!("Failed to get plot summary at `{directory:?}'"))
            }
        })
        .collect::<anyhow::Result<_>>()?;

        Ok(Info {
            plots_info,
            version: format!("{}-{}", env!("CARGO_PKG_VERSION"), env!("GIT_HASH")),
            reward_address: self.reward_address,
            sector_size: subspace_core_primitives::PLOT_SECTOR_SIZE,
        })
    }

    /// Iterate over plots
    pub async fn iter_plots(&'_ self) -> impl Iterator<Item = &'_ Plot> + '_ {
        self.plot_info.values()
    }

    /// Stops farming, closes plots, and sends signal to the node
    pub async fn close(self) -> anyhow::Result<()> {
        let mut maybe_cmd_sender = self.cmd_sender.lock().await;
        let Some(cmd_sender) = maybe_cmd_sender.take() else {
            return Ok(());
        };
        let _ = cmd_sender.send(());
        (&mut *self.handle.lock().await).await?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::{chain_spec, Node, Role};
    use tempfile::TempDir;

    #[tokio::test(flavor = "multi_thread")]
    async fn test_get_info() {
        let dir = TempDir::new().unwrap();
        let node = Node::builder()
            .force_authoring(true)
            .role(Role::Authority)
            .build(dir.path(), chain_spec::dev_config().unwrap())
            .await
            .unwrap();
        let plot_dir = TempDir::new().unwrap();
        let plots = [PlotDescription::new(plot_dir.as_ref(), bytesize::ByteSize::mib(32)).unwrap()];
        let cache_dir = TempDir::new().unwrap();
        let farmer = Farmer::builder()
            .build(
                Default::default(),
                node.clone(),
                &plots,
                CacheDescription::new(cache_dir.as_ref(), ByteSize::mib(32)).unwrap(),
            )
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;

        let Info {
            reward_address,
            plots_info,
            ..
        } = farmer.get_info().await.unwrap();
        assert_eq!(reward_address, Default::default());
        assert_eq!(plots_info.len(), 1);
        assert_eq!(
            plots_info[plot_dir.as_ref()].allocated_space,
            bytesize::ByteSize::mib(32)
        );

        farmer.close().await.unwrap();
        node.close().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_track_progress() {
        let dir = TempDir::new().unwrap();
        let node = Node::builder()
            .force_authoring(true)
            .role(Role::Authority)
            .build(dir.path(), chain_spec::dev_config().unwrap())
            .await
            .unwrap();
        let (plot_dir, cache_dir) = (TempDir::new().unwrap(), TempDir::new().unwrap());
        let n_sectors = 1;
        let farmer =
            Farmer::builder()
                .build(
                    Default::default(),
                    node.clone(),
                    &[PlotDescription::new(
                        plot_dir.as_ref(),
                        bytesize::ByteSize::mib(32 * n_sectors),
                    )
                    .unwrap()],
                    CacheDescription::new(cache_dir.as_ref(), ByteSize::mib(32)).unwrap(),
                )
                .await
                .unwrap();

        let progress = farmer
            .iter_plots()
            .await
            .next()
            .unwrap()
            .subscribe_initial_plotting_progress()
            .await
            .take(n_sectors as usize)
            .collect::<Vec<_>>()
            .await;
        assert_eq!(progress.len(), n_sectors as usize);

        farmer.close().await.unwrap();
        node.close().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_new_solution() {
        let dir = TempDir::new().unwrap();
        let node = Node::builder()
            .force_authoring(true)
            .role(Role::Authority)
            .build(dir.path(), chain_spec::dev_config().unwrap())
            .await
            .unwrap();
        let (plot_dir, cache_dir) = (TempDir::new().unwrap(), TempDir::new().unwrap());
        let farmer = Farmer::builder()
            .build(
                Default::default(),
                node.clone(),
                &[PlotDescription::new(plot_dir.as_ref(), bytesize::ByteSize::mib(32)).unwrap()],
                CacheDescription::new(cache_dir.as_ref(), ByteSize::mib(32)).unwrap(),
            )
            .await
            .unwrap();

        farmer
            .iter_plots()
            .await
            .next()
            .unwrap()
            .subscribe_new_solutions()
            .await
            .next()
            .await
            .expect("Farmer should send new solutions");

        farmer.close().await.unwrap();
        node.close().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_progress_restart() {
        let dir = TempDir::new().unwrap();
        let node = Node::builder()
            .force_authoring(true)
            .role(Role::Authority)
            .build(dir.path(), chain_spec::dev_config().unwrap())
            .await
            .unwrap();
        let (plot_dir, cache_dir) = (TempDir::new().unwrap(), TempDir::new().unwrap());
        let farmer = Farmer::builder()
            .build(
                Default::default(),
                node.clone(),
                &[PlotDescription::new(plot_dir.as_ref(), bytesize::ByteSize::mib(32)).unwrap()],
                CacheDescription::new(cache_dir.as_ref(), ByteSize::mib(32)).unwrap(),
            )
            .await
            .unwrap();

        let plot = farmer.iter_plots().await.next().unwrap();

        plot.subscribe_initial_plotting_progress()
            .await
            .for_each(|_| async {})
            .await;

        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            plot.subscribe_initial_plotting_progress()
                .await
                .for_each(|_| async {}),
        )
        .await
        .unwrap();

        farmer.close().await.unwrap();
        node.close().await;
    }
}

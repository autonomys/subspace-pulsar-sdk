use std::{
    collections::HashMap,
    io,
    num::NonZeroUsize,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::Context;
use bytesize::ByteSize;
use futures::{prelude::*, stream::FuturesUnordered};
use libp2p_core::Multiaddr;
use subspace_core_primitives::{PieceIndexHash, SectorIndex};
use subspace_networking::{Node as DSNNode, NodeRunner as DSNNodeRunner};
use subspace_rpc_primitives::SolutionResponse;
use tokio::sync::{oneshot, watch, Mutex};

use crate::{Node, PublicKey};

use subspace_farmer::{
    single_disk_plot::{
        piece_reader::PieceReader, plotting::PlottedSector, SingleDiskPlot, SingleDiskPlotError,
        SingleDiskPlotId, SingleDiskPlotInfo, SingleDiskPlotOptions, SingleDiskPlotSummary,
    },
    RpcClient,
};

/// Description of the plot
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlotDescription {
    /// Path of the plot
    pub directory: PathBuf,
    /// Space which you want to pledge
    pub space_pledged: ByteSize,
}

impl PlotDescription {
    /// Construct Plot description
    pub fn new(directory: impl Into<PathBuf>, space_pledged: ByteSize) -> Self {
        Self {
            directory: directory.into(),
            space_pledged,
        }
    }

    /// Wipe all the data from the plot
    pub async fn wipe(self) -> io::Result<()> {
        tokio::fs::remove_dir_all(self.directory).await
    }
}

struct DSNRecordsCacheSize(NonZeroUsize);

impl Default for DSNRecordsCacheSize {
    fn default() -> Self {
        Self(NonZeroUsize::new(32678).unwrap())
    }
}

/// Farmer builder
#[derive(Default)]
pub struct Builder {
    listen_on: Vec<Multiaddr>,
    bootstrap_nodes: Vec<Multiaddr>,
    dsn_records_cache_db: Option<PathBuf>,
    dsn_records_cache_size: DSNRecordsCacheSize,
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

async fn configure_dsn(
    records_cache_db: PathBuf,
    listen_on: Vec<Multiaddr>,
    bootstrap_nodes: Vec<Multiaddr>,
    record_cache_size: NonZeroUsize,
    readers_and_pieces: &Arc<std::sync::Mutex<Option<ReadersAndPieces>>>,
) -> Result<
    (
        Option<DSNNode>,
        Option<DSNNodeRunner<ConfiguredRecordStore>>,
    ),
    BuildError,
> {
    use subspace_networking::{
        BootstrappedNetworkingParameters, Config, CustomRecordStore,
        LimitedSizeRecordStorageWrapper, MemoryProviderStorage, ParityDbRecordStorage,
        PieceByHashRequestHandler, PieceByHashResponse, PieceKey,
    };

    if !listen_on.is_empty() {
        return Ok((None, None));
    }

    let weak_readers_and_pieces = Arc::downgrade(readers_and_pieces);

    let handle = tokio::runtime::Handle::current();
    let default_config = Config::with_generated_keypair();

    let config = Config::<ConfiguredRecordStore> {
        listen_on,
        allow_non_globals_in_dht: true,
        networking_parameters_registry: BootstrappedNetworkingParameters::new(bootstrap_nodes)
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
        record_store: CustomRecordStore::new(
            LimitedSizeRecordStorageWrapper::new(
                ParityDbRecordStorage::new(&records_cache_db)?,
                record_cache_size,
            ),
            MemoryProviderStorage::default(),
        ),
        ..default_config
    };

    subspace_networking::create(config)
        .await
        .map(|(node, node_runner)| (Some(node), Some(node_runner)))
        .map_err(Into::into)
}

impl Builder {
    /// Construct new builder
    pub fn new() -> Self {
        Self::default()
    }

    /// DSN would listen on the addresses supplied
    pub fn listen_on(mut self, multiaddrs: impl IntoIterator<Item = Multiaddr>) -> Self {
        self.listen_on = multiaddrs.into_iter().collect();
        self
    }

    /// Connect to those nodes apart from ones from chain spec
    pub fn bootstrap_nodes(mut self, multiaddrs: impl IntoIterator<Item = Multiaddr>) -> Self {
        self.bootstrap_nodes = multiaddrs.into_iter().collect();
        self
    }

    /// Path to dsn records cache. BEWARE: DSN is disabled if you don't supply this
    pub fn dsn_records_cache_db(mut self, path: impl AsRef<Path>) -> Self {
        self.dsn_records_cache_db = Some(path.as_ref().to_owned());
        self
    }

    /// Set dsn records cache size
    pub fn dsn_records_cache_size(mut self, size: NonZeroUsize) -> Self {
        self.dsn_records_cache_size = DSNRecordsCacheSize(size);
        self
    }

    /// Open and start farmer
    pub async fn build(
        self,
        reward_address: PublicKey,
        node: Node,
        plots: &[PlotDescription],
    ) -> Result<Farmer, BuildError> {
        if plots.is_empty() {
            return Err(BuildError::NoPlotsSupplied);
        }
        let Self {
            listen_on,
            bootstrap_nodes,
            dsn_records_cache_db,
            dsn_records_cache_size: DSNRecordsCacheSize(dsn_records_cache_size),
        } = self;

        let mut single_disk_plots = Vec::with_capacity(plots.len());
        let mut plot_info = HashMap::with_capacity(plots.len());
        let (dsn_node, dsn_node_runner) = if let Some(dsn_records_cache_db) = dsn_records_cache_db {
            configure_dsn(
                dsn_records_cache_db,
                listen_on,
                bootstrap_nodes,
                dsn_records_cache_size,
                &Arc::new(std::sync::Mutex::new(None)),
            )
            .await?
        } else {
            (None, None)
        };

        let space_l = node
            .farmer_protocol_info()
            .await
            .map_err(BuildError::RPCError)?
            .space_l;

        for description in plots {
            let directory = description.directory.clone();
            let allocated_space = description.space_pledged.as_u64();
            let description = SingleDiskPlotOptions {
                allocated_space,
                directory: directory.clone(),
                reward_address: *reward_address,
                rpc_client: node.clone(),
                dsn_node: dsn_node.clone(),
            };
            let single_disk_plot =
                tokio::task::spawn_blocking(move || SingleDiskPlot::new(description))
                    .await
                    .expect("Single disk plot never panics")?;

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
                    total_sectors: allocated_space
                        / subspace_core_primitives::plot_sector_size(space_l),
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

        if let Some(mut node_runner) = dsn_node_runner {
            tokio::spawn(async move {
                node_runner.run().await;
            });
        }

        let (cmd_sender, cmd_receiver) = oneshot::channel::<oneshot::Sender<()>>();

        tokio::task::spawn_blocking({
            let handle = tokio::runtime::Handle::current();
            move || {
                let result_maybe_sender = handle.block_on(futures::future::select(
                    single_disk_plots_stream.next(),
                    cmd_receiver,
                ));
                match result_maybe_sender {
                    future::Either::Left((maybe_result, _receiver)) => {
                        maybe_result.unwrap().unwrap()
                    }
                    future::Either::Right((maybe_sender, future)) => {
                        drop(future);
                        drop(single_disk_plots_stream);
                        if let Ok(sender) = maybe_sender {
                            let _ = sender.send(());
                        }
                    }
                }
            }
        });

        Ok(Farmer {
            cmd_sender: Arc::new(Mutex::new(Some(cmd_sender))),
            reward_address,
            plot_info: Arc::new(plot_info),
            node,
        })
    }
}

/// Farmer structure
#[derive(Clone, Debug)]
pub struct Farmer {
    cmd_sender: Arc<Mutex<Option<oneshot::Sender<oneshot::Sender<()>>>>>,
    reward_address: PublicKey,
    plot_info: Arc<HashMap<PathBuf, Plot>>,
    node: Node,
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
        Builder::new()
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
            sector_size: self
                .node
                .farmer_protocol_info()
                .await
                .map(|info| subspace_core_primitives::plot_sector_size(info.space_l))
                .map_err(|err| anyhow::anyhow!("Failed to get farmer protocol info: {err}"))?,
        })
    }

    /// Iterate over plots
    pub async fn iter_plots(&'_ self) -> impl Iterator<Item = &'_ Plot> + '_ {
        self.plot_info.values()
    }

    /// Stops farming, closes plots, and sends signal to the node
    pub async fn close(self) {
        let mut maybe_cmd_sender = self.cmd_sender.lock().await;
        let Some(cmd_sender) = maybe_cmd_sender.take() else {
            return;
        };
        let (stop_sender, stop_receiver) = oneshot::channel();
        if cmd_sender.send(stop_sender).is_err() {
            return;
        }

        stop_receiver
            .await
            .expect("We should always receive here, as task is alive");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::{chain_spec, Node};
    use tempdir::TempDir;

    #[tokio::test(flavor = "multi_thread")]
    async fn test_get_info() {
        let dir = TempDir::new("test").unwrap();
        let node = Node::builder()
            .force_authoring(true)
            .role(sc_service::Role::Authority)
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

        let Info {
            reward_address,
            plots_info,
            ..
        } = farmer.get_info().await.unwrap();
        assert_eq!(reward_address, Default::default());
        assert_eq!(plots_info.len(), 1);
        assert_eq!(
            plots_info[plot_dir.as_ref()].allocated_space,
            bytesize::ByteSize::mb(10)
        );

        farmer.close().await;
        node.close().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_track_progress() {
        let dir = TempDir::new("test").unwrap();
        let node = Node::builder()
            .force_authoring(true)
            .role(sc_service::Role::Authority)
            .build(dir.path(), chain_spec::dev_config().unwrap())
            .await
            .unwrap();
        let plot_dir = TempDir::new("test").unwrap();
        let n_sectors = 1;
        let farmer = Farmer::builder()
            .build(
                Default::default(),
                node.clone(),
                &[PlotDescription::new(
                    plot_dir.as_ref(),
                    bytesize::ByteSize::mib(4 * n_sectors),
                )],
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

        farmer.close().await;
        node.close().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_new_solution() {
        let dir = TempDir::new("test").unwrap();
        let node = Node::builder()
            .force_authoring(true)
            .role(sc_service::Role::Authority)
            .build(dir.path(), chain_spec::dev_config().unwrap())
            .await
            .unwrap();
        let plot_dir = TempDir::new("test").unwrap();
        let farmer = Farmer::builder()
            .build(
                Default::default(),
                node.clone(),
                &[PlotDescription::new(
                    plot_dir.as_ref(),
                    bytesize::ByteSize::mib(4),
                )],
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

        farmer.close().await;
        node.close().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_progress_restart() {
        let dir = TempDir::new("test").unwrap();
        let node = Node::builder()
            .force_authoring(true)
            .role(sc_service::Role::Authority)
            .build(dir.path(), chain_spec::dev_config().unwrap())
            .await
            .unwrap();
        let plot_dir = TempDir::new("test").unwrap();
        let farmer = Farmer::builder()
            .build(
                Default::default(),
                node.clone(),
                &[PlotDescription::new(
                    plot_dir.as_ref(),
                    bytesize::ByteSize::mib(4),
                )],
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

        farmer.close().await;
        node.close().await;
    }
}

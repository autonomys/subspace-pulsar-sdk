use std::collections::HashMap;
use std::io;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
pub use builder::{Builder, Config};
use bytesize::ByteSize;
use futures::prelude::*;
use futures::stream::FuturesUnordered;
use serde::{Deserialize, Serialize};
use subspace_core_primitives::{PieceIndexHash, SectorIndex, PLOT_SECTOR_SIZE};
use subspace_farmer::single_disk_plot::{
    SingleDiskPlot, SingleDiskPlotError, SingleDiskPlotId, SingleDiskPlotInfo,
    SingleDiskPlotOptions, SingleDiskPlotSummary,
};
use subspace_farmer_components::plotting::PlottedSector;
use subspace_networking::{LimitedSizeRecordStorageWrapper, ParityDbRecordStorage};
use subspace_rpc_primitives::SolutionResponse;
use tokio::sync::{oneshot, watch, Mutex};

use crate::networking::{FarmerRecordStorage, PieceDetails, ReadersAndPieces};
use crate::{Node, PublicKey};

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
    fn record_storage(
        self,
        peer_id: subspace_networking::libp2p::PeerId,
    ) -> parity_db::Result<FarmerRecordStorage> {
        ParityDbRecordStorage::new(&self.directory).map(|storage| {
            LimitedSizeRecordStorageWrapper::new(
                storage,
                NonZeroUsize::new(self.space_dedicated.as_u64() as _)
                    .expect("Always more than 1Mib"),
                peer_id,
            )
        })
    }
}

/// Error type for cache description constructor
#[derive(Debug, Clone, Copy, thiserror::Error)]
#[error("Cache should be larger than {}", CacheDescription::MIN_SIZE)]
pub struct CacheTooSmall;

impl CacheDescription {
    /// Minimal cache size
    pub const MIN_SIZE: ByteSize = ByteSize::mib(1);

    /// Construct Plot description
    pub fn new(
        directory: impl Into<PathBuf>,
        space_dedicated: ByteSize,
    ) -> Result<Self, CacheTooSmall> {
        if space_dedicated < Self::MIN_SIZE {
            return Err(CacheTooSmall);
        }
        Ok(Self { directory: directory.into(), space_dedicated })
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

/// Error type for cache description constructor
#[derive(Debug, Clone, Copy, thiserror::Error)]
#[error("Cache should be larger than {}", PlotDescription::MIN_SIZE)]
pub struct PlotConstructionError;

impl PlotDescription {
    /// Minimal plot size
    pub const MIN_SIZE: ByteSize = ByteSize::b(PLOT_SECTOR_SIZE + Self::SECTOR_OVERHEAD.0);
    // TODO: Account for prefix and metadata sizes
    const SECTOR_OVERHEAD: ByteSize = ByteSize::mb(2);

    /// Construct Plot description
    pub fn new(
        directory: impl Into<PathBuf>,
        space_pledged: ByteSize,
    ) -> Result<Self, PlotConstructionError> {
        if space_pledged < Self::MIN_SIZE {
            return Err(PlotConstructionError);
        }
        Ok(Self { directory: directory.into(), space_pledged })
    }

    /// Wipe all the data from the plot
    pub async fn wipe(self) -> io::Result<()> {
        tokio::fs::remove_dir_all(self.directory).await
    }
}

mod builder {
    use std::num::NonZeroUsize;

    use derivative::Derivative;
    use derive_builder::Builder;
    use serde::{Deserialize, Serialize};

    use super::{BuildError, CacheDescription};
    use crate::{Farmer, Node, PlotDescription, PublicKey};

    fn default_piece_receiver_batch_size() -> NonZeroUsize {
        NonZeroUsize::new(12).unwrap()
    }

    fn default_piece_publisher_batch_size() -> NonZeroUsize {
        NonZeroUsize::new(12).unwrap()
    }

    fn default_max_concurrent_plots() -> NonZeroUsize {
        NonZeroUsize::new(10).unwrap()
    }

    /// Technical type which stores all
    #[derive(Debug, Clone, Derivative, Builder, Serialize, Deserialize)]
    #[derivative(Default)]
    #[builder(pattern = "immutable", build_fn(private, name = "_build"), name = "Builder")]
    #[non_exhaustive]
    pub struct Config {
        /// Defines size for the pieces batch of the piece receiving process.
        #[builder(default = "default_piece_receiver_batch_size()")]
        #[derivative(Default(value = "default_piece_receiver_batch_size()"))]
        #[serde(default = "default_piece_receiver_batch_size")]
        pub piece_receiver_batch_size: NonZeroUsize,
        /// Defines size for the pieces batch of the piece publishing process.
        #[builder(default = "default_piece_publisher_batch_size()")]
        #[derivative(Default(value = "default_piece_publisher_batch_size()"))]
        #[serde(default = "default_piece_publisher_batch_size")]
        pub piece_publisher_batch_size: NonZeroUsize,
        /// Number of plots that can be plotted concurrently, impacts RAM usage.
        #[builder(default = "default_max_concurrent_plots()")]
        #[derivative(Default(value = "default_max_concurrent_plots()"))]
        #[serde(default = "default_max_concurrent_plots")]
        pub max_concurrent_plots: NonZeroUsize,
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
            self.configuration().build(reward_address, node, plots, cache).await
        }
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
    /// Failed to fetch data from the node
    #[error("Failed to fetch data from node: {0}")]
    RPCError(#[source] subspace_farmer::RpcClientError),
    /// Failed to create parity db record storage
    #[error("Failed to create parity db record storage: {0}")]
    ParityDbError(#[from] parity_db::Error),
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
        if plots.is_empty() {
            return Err(BuildError::NoPlotsSupplied);
        }

        let Self { piece_receiver_batch_size, piece_publisher_batch_size, max_concurrent_plots } =
            self;

        let mut single_disk_plots = Vec::with_capacity(plots.len());
        let mut plot_info = HashMap::with_capacity(plots.len());

        let piece_publisher_semaphore =
            Arc::new(tokio::sync::Semaphore::new(piece_receiver_batch_size.get()));
        let piece_receiver_semaphore =
            Arc::new(tokio::sync::Semaphore::new(piece_publisher_batch_size.get()));
        let concurrent_plotting_semaphore =
            Arc::new(tokio::sync::Semaphore::new(max_concurrent_plots.get()));

        node.farmer_record_storage.swap(cache.record_storage(node.dsn_node.id())?);

        for description in plots {
            let directory = description.directory.clone();
            let allocated_space = description.space_pledged.as_u64();
            let description = SingleDiskPlotOptions {
                allocated_space,
                directory: directory.clone(),
                reward_address: *reward_address,
                rpc_client: node.clone(),
                dsn_node: node.dsn_node.clone(),
                piece_receiver_semaphore: Arc::clone(&piece_receiver_semaphore),
                piece_publisher_semaphore: Arc::clone(&piece_publisher_semaphore),
                concurrent_plotting_semaphore: Arc::clone(&concurrent_plotting_semaphore),
            };
            let single_disk_plot = SingleDiskPlot::new(description).await?;

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

        let readers_and_pieces = ReadersAndPieces::new(&single_disk_plots).await;
        node.readers_and_pieces
            .lock()
            .expect("Readers and pieces can't poison lock")
            .replace(readers_and_pieces);

        for (plot_offset, single_disk_plot) in single_disk_plots.iter().enumerate() {
            let readers_and_pieces = Arc::clone(&node.readers_and_pieces);

            // Collect newly plotted pieces
            // TODO: Once we have replotting, this will have to be updated
            single_disk_plot
                .on_sector_plotted(Arc::new(move |plotted_sector| {
                    readers_and_pieces
                        .lock()
                        .expect("Readers and pieces can't poison lock")
                        .as_mut()
                        .expect("Initial value was populated above; qed")
                        .extend(plotted_sector.piece_indexes.iter().copied().enumerate().map(
                            |(piece_offset, piece_index)| {
                                (
                                    PieceIndexHash::from_index(piece_index),
                                    PieceDetails {
                                        plot_offset,
                                        sector_index: plotted_sector.sector_index,
                                        piece_offset: piece_offset as u64,
                                    },
                                )
                            },
                        ));
                }))
                .detach();
        }

        let mut single_disk_plots_stream =
            single_disk_plots.into_iter().map(SingleDiskPlot::run).collect::<FuturesUnordered<_>>();

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
                    future::Either::Left((maybe_result, _)) =>
                        maybe_result.expect("There is at least one plot"),
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
    /// Multiple plots can reuse the same identity, but they have to use
    /// different ranges for sector indexes or else they'll essentially plot
    /// the same data and will not result in increased probability of
    /// winning the reward.
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

    /// Will return a stream of initial plotting progress which will end once we
    /// finish plotting
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
            .take_while(|InitialPlottingProgress { current_sector, total_sectors, .. }| {
                futures::future::ready(current_sector != total_sectors)
            })
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
            || dirs.into_iter().map(SingleDiskPlot::collect_summary).collect::<Vec<_>>()
        })
        .await?
        .into_iter()
        .map(|summary| match summary {
            SingleDiskPlotSummary::Found { info, directory } => Ok((directory, info.into())),
            SingleDiskPlotSummary::NotFound { directory } =>
                Err(anyhow::anyhow!("Didn't found plot at `{directory:?}'")),
            SingleDiskPlotSummary::Error { directory, error } =>
                Err(error).context(format!("Failed to get plot summary at `{directory:?}'")),
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
    use tempfile::TempDir;

    use super::*;
    use crate::node::{chain_spec, Node, Role};

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
        let plots = [PlotDescription::new(plot_dir.as_ref(), PlotDescription::MIN_SIZE).unwrap()];
        let cache_dir = TempDir::new().unwrap();
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

        let Info { reward_address, plots_info, .. } = farmer.get_info().await.unwrap();
        assert_eq!(reward_address, Default::default());
        assert_eq!(plots_info.len(), 1);
        assert_eq!(plots_info[plot_dir.as_ref()].allocated_space, PlotDescription::MIN_SIZE);

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
        let farmer = Farmer::builder()
            .build(
                Default::default(),
                node.clone(),
                &[PlotDescription::new(
                    plot_dir.as_ref(),
                    ByteSize::b(PlotDescription::MIN_SIZE.as_u64() * n_sectors),
                )
                .unwrap()],
                CacheDescription::new(cache_dir.as_ref(), CacheDescription::MIN_SIZE).unwrap(),
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
                &[PlotDescription::new(plot_dir.as_ref(), PlotDescription::MIN_SIZE).unwrap()],
                CacheDescription::new(cache_dir.as_ref(), CacheDescription::MIN_SIZE).unwrap(),
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
                &[PlotDescription::new(plot_dir.as_ref(), PlotDescription::MIN_SIZE).unwrap()],
                CacheDescription::new(cache_dir.as_ref(), CacheDescription::MIN_SIZE).unwrap(),
            )
            .await
            .unwrap();

        let plot = farmer.iter_plots().await.next().unwrap();

        plot.subscribe_initial_plotting_progress().await.for_each(|_| async {}).await;

        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            plot.subscribe_initial_plotting_progress().await.for_each(|_| async {}),
        )
        .await
        .unwrap();

        farmer.close().await.unwrap();
        node.close().await;
    }
}

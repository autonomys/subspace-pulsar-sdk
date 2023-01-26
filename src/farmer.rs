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
use subspace_core_primitives::crypto::kzg;
use subspace_core_primitives::{PieceIndexHash, SectorIndex, PLOT_SECTOR_SIZE};
use subspace_farmer::single_disk_plot::{
    SingleDiskPlot, SingleDiskPlotError, SingleDiskPlotId, SingleDiskPlotInfo,
    SingleDiskPlotOptions, SingleDiskPlotSummary,
};
use subspace_farmer::utils::farmer_piece_cache::FarmerPieceCache;
use subspace_farmer::utils::farmer_piece_getter::FarmerPieceGetter;
use subspace_farmer::utils::node_piece_getter::NodePieceGetter;
use subspace_farmer::utils::parity_db_store::ParityDbStore;
use subspace_farmer::utils::readers_and_pieces::{PieceDetails, ReadersAndPieces};
use subspace_farmer_components::plotting::PlottedSector;
use subspace_networking::utils::multihash::ToMultihash;
use subspace_networking::{ParityDbProviderStorage, PieceByHashRequest, PieceByHashResponse};
use subspace_rpc_primitives::SolutionResponse;
use tokio::sync::{oneshot, watch, Mutex};

use crate::networking::FarmerProviderStorage;
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
    use derive_more::{Deref, DerefMut, Display, From};
    use serde::{Deserialize, Serialize};

    use super::{BuildError, CacheDescription};
    use crate::{Farmer, Node, PlotDescription, PublicKey};

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
    pub struct MaxConcurrentPlots(
        #[derivative(Default(value = "NonZeroUsize::new(10).expect(\"10 > 0\")"))]
        pub(crate)  NonZeroUsize,
    );

    /// Technical type which stores all
    #[derive(Debug, Clone, Derivative, Builder, Serialize, Deserialize)]
    #[derivative(Default)]
    #[builder(pattern = "immutable", build_fn(private, name = "_build"), name = "Builder")]
    #[non_exhaustive]
    pub struct Config {
        /// Number of plots that can be plotted concurrently, impacts RAM usage.
        #[builder(default, setter(into))]
        #[serde(default, skip_serializing_if = "crate::utils::is_default")]
        pub max_concurrent_plots: MaxConcurrentPlots,
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
    /// Other error
    #[error("{0}")]
    Other(#[from] anyhow::Error),
}

pub(crate) fn get_piece_by_hash(
    PieceByHashRequest { piece_index_hash }: &PieceByHashRequest,
    piece_cache: &ParityDbStore,
    weak_readers_and_pieces: &std::sync::Weak<parking_lot::Mutex<Option<ReadersAndPieces>>>,
    handle: &tokio::runtime::Handle,
) -> Option<PieceByHashResponse> {
    use tracing::debug;

    let result = {
        debug!(?piece_index_hash, "Piece request received. Trying cache...");
        let multihash = piece_index_hash.to_multihash();

        let piece_from_cache = piece_cache.get(&multihash.into());

        if piece_from_cache.is_some() {
            piece_from_cache
        } else {
            debug!(?piece_index_hash, "No piece in the cache. Trying archival storage...");

            let read_piece_fut = {
                let readers_and_pieces = match weak_readers_and_pieces.upgrade() {
                    Some(readers_and_pieces) => readers_and_pieces,
                    None => {
                        debug!("A readers and pieces are already dropped");
                        return None;
                    }
                };
                let readers_and_pieces = readers_and_pieces.lock();
                let readers_and_pieces = match readers_and_pieces.as_ref() {
                    Some(readers_and_pieces) => readers_and_pieces,
                    None => {
                        debug!(?piece_index_hash, "Readers and pieces are not initialized yet");
                        return None;
                    }
                };

                readers_and_pieces.read_piece(piece_index_hash)?
            };

            tokio::task::block_in_place(move || handle.block_on(read_piece_fut))
        }
    };

    Some(PieceByHashResponse { piece: result })
}

const RECORDS_ROOTS_CACHE_SIZE: NonZeroUsize = NonZeroUsize::new(1_000_000).expect("Not zero; qed");

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

        let Self { max_concurrent_plots } = self;

        let mut single_disk_plots = Vec::with_capacity(plots.len());
        let mut plot_info = HashMap::with_capacity(plots.len());

        let concurrent_plotting_semaphore =
            Arc::new(tokio::sync::Semaphore::new(max_concurrent_plots.get()));

        let base_path = cache.directory;
        let readers_and_pieces = Arc::clone(&node.farmer_readers_and_pieces);
        let piece_cache_size = NonZeroUsize::new(100).expect("100 > 0. TODO: put propper value");

        let piece_cache = {
            let piece_cache_db_path = base_path.join("piece_cache_db");
            let provider_cache_db_path = base_path.join("provider_cache_db");
            let provider_cache_size =
                piece_cache_size.saturating_mul(NonZeroUsize::new(10).expect("10 > 0")); // TODO: add proper value

            tracing::info!(
                ?piece_cache_db_path,
                ?piece_cache_size,
                ?provider_cache_db_path,
                ?provider_cache_size,
                "Record cache DB configured."
            );

            let peer_id = node.dsn_node.id();

            let db_provider_storage =
                ParityDbProviderStorage::new(&provider_cache_db_path, provider_cache_size, peer_id)
                    .map_err(|err| anyhow::anyhow!(err.to_string()))?;

            node.farmer_provider_storage.swap(FarmerProviderStorage::new(
                peer_id,
                readers_and_pieces.clone(),
                db_provider_storage,
            ));

            let piece_store = ParityDbStore::new(&piece_cache_db_path)
                .map_err(|err| anyhow::anyhow!(err.to_string()))?;
            *node.farmer_piece_store.lock() = Some(piece_store.clone());

            FarmerPieceCache::new(piece_store.clone(), piece_cache_size, peer_id)
        };

        let piece_cache = Arc::new(tokio::sync::Mutex::new(piece_cache));
        crate::networking::start_announcements_processor(
            node.dsn_node.clone(),
            Arc::clone(&piece_cache),
            Arc::downgrade(&readers_and_pieces),
        )
        .expect("Add error handling")
        .detach();

        let kzg = kzg::Kzg::new(kzg::test_public_parameters());
        let records_roots_cache =
            parking_lot::Mutex::new(lru::LruCache::new(RECORDS_ROOTS_CACHE_SIZE));
        let piece_provider = subspace_networking::utils::piece_provider::PieceProvider::new(
            node.dsn_node.clone(),
            Some(subspace_farmer::utils::piece_validator::RecordsRootPieceValidator::new(
                node.dsn_node.clone(),
                node.clone(),
                kzg.clone(),
                records_roots_cache,
            )),
        );
        let piece_getter = Arc::new(FarmerPieceGetter::new(
            NodePieceGetter::new(piece_provider),
            piece_cache,
            node.dsn_node.clone(),
        ));

        for description in plots {
            let directory = description.directory.clone();
            let allocated_space = description.space_pledged.as_u64();
            let description = SingleDiskPlotOptions {
                allocated_space,
                directory: directory.clone(),
                reward_address: *reward_address,
                node_client: node.clone(),
                kzg: kzg.clone(),
                piece_getter: piece_getter.clone(),
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

        let new_readers_and_pieces = {
            // Store piece readers so we can reference them later
            let readers = single_disk_plots.iter().map(SingleDiskPlot::piece_reader).collect();

            tracing::debug!("Collecting already plotted pieces");

            // Collect already plotted pieces
            let plotted_pieces: HashMap<PieceIndexHash, PieceDetails> = single_disk_plots
                .iter()
                .enumerate()
                .flat_map(|(plot_offset, single_disk_plot)| {
                    single_disk_plot
                        .plotted_sectors()
                        .enumerate()
                        .filter_map(move |(sector_offset, plotted_sector_result)| {
                            match plotted_sector_result {
                                Ok(plotted_sector) => Some(plotted_sector),
                                Err(error) => {
                                    tracing::error!(
                                        %error,
                                        %plot_offset,
                                        %sector_offset,
                                        "Failed reading plotted sector on startup, skipping"
                                    );
                                    None
                                }
                            }
                        })
                        .flat_map(move |plotted_sector| {
                            plotted_sector.piece_indexes.into_iter().enumerate().map(
                                move |(piece_offset, piece_index)| {
                                    (
                                        PieceIndexHash::from_index(piece_index),
                                        PieceDetails {
                                            plot_offset,
                                            sector_index: plotted_sector.sector_index,
                                            piece_offset: piece_offset as u64,
                                        },
                                    )
                                },
                            )
                        })
                })
                // We implicitly ignore duplicates here, reading just from one of the plots
                .collect();
            tracing::debug!("Finished collecting already plotted pieces");

            ReadersAndPieces::new(readers, plotted_pieces)
        };
        readers_and_pieces.lock().replace(new_readers_and_pieces);

        for (plot_offset, single_disk_plot) in single_disk_plots.iter().enumerate() {
            let readers_and_pieces = Arc::clone(&readers_and_pieces);

            // We are not going to send anything here, but dropping of sender on dropping of
            // corresponding `SingleDiskPlot` will allow us to stop background tasks.
            let (dropped_sender, _dropped_receiver) = tokio::sync::broadcast::channel::<()>(1);

            let node = node.dsn_node.clone();
            // Collect newly plotted pieces
            // TODO: Once we have replotting, this will have to be updated
            single_disk_plot
                .on_sector_plotted(Arc::new(move |(plotted_sector, plotting_permit)| {
                    let plotting_permit = Arc::clone(plotting_permit);
                    let sector_index = plotted_sector.sector_index;

                    let mut dropped_receiver = dropped_sender.subscribe();

                    let new_pieces = {
                        let mut readers_and_pieces = readers_and_pieces.lock();
                        let readers_and_pieces = readers_and_pieces
                            .as_mut()
                            .expect("Initial value was populated above; qed");

                        let new_pieces = plotted_sector
                            .piece_indexes
                            .iter()
                            .filter(|&&piece_index| {
                                // Skip pieces that are already plotted and thus were announced
                                // before
                                !readers_and_pieces
                                    .contains_piece(&PieceIndexHash::from_index(piece_index))
                            })
                            .copied()
                            .collect::<Vec<_>>();

                        readers_and_pieces.add_pieces(
                            plotted_sector.piece_indexes.iter().copied().enumerate().map(
                                |(piece_offset, piece_index)| {
                                    (
                                        PieceIndexHash::from_index(piece_index),
                                        PieceDetails {
                                            plot_offset,
                                            sector_index,
                                            piece_offset: piece_offset as u64,
                                        },
                                    )
                                },
                            ),
                        );

                        new_pieces
                    };

                    if new_pieces.is_empty() {
                        // None of the pieces are new, nothing left to do here
                        return;
                    }

                    let node = node.clone();
                    let publish_fut = async move {
                        let mut pieces_publishing_futures = new_pieces
                            .into_iter()
                            .map(|piece_index| {
                                subspace_networking::utils::pieces::announce_single_piece_index_with_backoff(piece_index, &node)
                            })
                            .collect::<FuturesUnordered<_>>();

                        while pieces_publishing_futures.next().await.is_some() {
                            // Nothing is needed here, just driving all futures
                            // to completion
                        }

                        tracing::info!("Piece publishing was successful.");

                        // Release only after publishing is finished
                        drop(plotting_permit);
                    };

                    tokio::spawn(async move {
                        use futures::future::{select, Either};

                        let result =
                            select(Box::pin(publish_fut), Box::pin(dropped_receiver.recv())).await;
                        if !matches!(result, Either::Right(_)) {
                            tracing::debug!("Piece publishing was cancelled due to shutdown.");
                        }
                    });
                }))
                .detach();
        }

        let mut single_disk_plots_stream =
            single_disk_plots.into_iter().map(SingleDiskPlot::run).collect::<FuturesUnordered<_>>();

        let (cmd_sender, cmd_receiver) = oneshot::channel::<()>();

        let handle = tokio::task::spawn_blocking({
            let node = node.clone();
            let handle = tokio::runtime::Handle::current();

            move || {
                use future::Either::*;

                match handle.block_on(future::select(single_disk_plots_stream.next(), cmd_receiver))
                {
                    Left((_, _)) if handle.block_on(node.is_closed()) => Ok(()),
                    Left((maybe_result, _)) =>
                        maybe_result.expect("there is always at least one plot"),
                    Right((_, _)) => Ok(()),
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
    progress: watch::Receiver<Option<(PlottedSector, Arc<tokio::sync::OwnedSemaphorePermit>)>>,
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
    #![allow(clippy::unwrap_used)]

    use tempfile::TempDir;

    use super::*;
    use crate::node::{chain_spec, Node};

    #[tokio::test(flavor = "multi_thread")]
    async fn test_get_info() {
        let dir = TempDir::new().unwrap();
        let node = Node::dev().build(dir.path(), chain_spec::dev_config().unwrap()).await.unwrap();
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
        let node = Node::dev().build(dir.path(), chain_spec::dev_config().unwrap()).await.unwrap();
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
        let node = Node::dev().build(dir.path(), chain_spec::dev_config().unwrap()).await.unwrap();
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
        let node = Node::dev().build(dir.path(), chain_spec::dev_config().unwrap()).await.unwrap();
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

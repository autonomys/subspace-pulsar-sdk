use std::collections::HashMap;
use std::io;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
pub use builder::{Builder, Config};
use derivative::Derivative;
use futures::prelude::*;
use futures::stream::FuturesUnordered;
use serde::{Deserialize, Serialize};
use subspace_core_primitives::crypto::kzg;
use subspace_core_primitives::{PieceIndexHash, SectorIndex, PLOT_SECTOR_SIZE};
use subspace_farmer::single_disk_plot::{
    SingleDiskPlot, SingleDiskPlotError, SingleDiskPlotId, SingleDiskPlotInfo,
    SingleDiskPlotOptions, SingleDiskPlotSummary,
};
use subspace_farmer::utils::farmer_piece_getter::FarmerPieceGetter;
use subspace_farmer::utils::node_piece_getter::NodePieceGetter as DsnPieceGetter;
use subspace_farmer::utils::parity_db_store::ParityDbStore;
use subspace_farmer::utils::piece_validator::SegmentCommitmentPieceValidator;
use subspace_farmer::utils::readers_and_pieces::{PieceDetails, ReadersAndPieces};
use subspace_farmer_components::piece_caching::PieceMemoryCache;
use subspace_farmer_components::plotting::PlottedSector;
use subspace_networking::utils::multihash::ToMultihash;
use subspace_networking::{ParityDbProviderStorage, PieceByHashResponse};
use subspace_rpc_primitives::SolutionResponse;
use tokio::sync::{oneshot, watch, Mutex};
use tracing_futures::Instrument;

use self::builder::{PieceCacheSize, ProvidedKeysLimit};
use crate::dsn::{FarmerPieceCache, FarmerProviderStorage, NodePieceGetter};
use crate::utils::{self, AsyncDropFutures, ByteSize, DropCollection};
use crate::{Node, PublicKey};

/// Description of the cache
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[non_exhaustive]
pub struct CacheDescription {
    /// Path to the cache description
    pub directory: PathBuf,
    /// Space which you want to dedicate
    pub space_dedicated: ByteSize,
}

/// Error type for cache description constructor
#[derive(Debug, Clone, Copy, thiserror::Error)]
#[error("Cache should be larger than {}", CacheDescription::MIN_SIZE)]
pub struct CacheTooSmall;

impl CacheDescription {
    /// Minimal cache size
    pub const MIN_SIZE: ByteSize = ByteSize::mib(1);

    /// Construct cache description
    pub fn new(
        directory: impl Into<PathBuf>,
        space_dedicated: ByteSize,
    ) -> Result<Self, CacheTooSmall> {
        if space_dedicated < Self::MIN_SIZE {
            return Err(CacheTooSmall);
        }
        Ok(Self { directory: directory.into(), space_dedicated })
    }

    /// Creates minimal cache description
    pub fn minimal(directory: impl Into<PathBuf>) -> Self {
        Self { directory: directory.into(), space_dedicated: Self::MIN_SIZE }
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
    pub space_pledged: ByteSize,
}

/// Error type for cache description constructor
#[derive(Debug, Clone, Copy, thiserror::Error)]
#[error("Cache should be larger than {}", PlotDescription::MIN_SIZE)]
pub struct PlotConstructionError;

impl PlotDescription {
    /// Minimal plot size
    pub const MIN_SIZE: ByteSize = ByteSize::b(PLOT_SECTOR_SIZE + Self::SECTOR_OVERHEAD.0 .0);
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

    /// Creates a minimal plot at specified path
    pub fn minimal(directory: impl Into<PathBuf>) -> Self {
        Self { directory: directory.into(), space_pledged: Self::MIN_SIZE }
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
    use crate::utils::ByteSize;
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
        #[derivative(Default(value = "ByteSize::mib(10)"))] pub(crate) ByteSize,
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
    pub struct ProvidedKeysLimit(
        #[derivative(Default(value = "NonZeroUsize::new(655360).expect(\"655360 > 0\")"))]
        pub(crate) NonZeroUsize,
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
        /// Number of plots that can be plotted concurrently, impacts RAM usage.
        #[builder(default, setter(into))]
        #[serde(default, skip_serializing_if = "crate::utils::is_default")]
        pub piece_cache_size: PieceCacheSize,
        /// Number of plots that can be plotted concurrently, impacts RAM usage.
        #[builder(default, setter(into))]
        #[serde(default, skip_serializing_if = "crate::utils::is_default")]
        pub provided_keys_limit: ProvidedKeysLimit,
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
            node: &Node,
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
    /// Other error
    #[error("{0}")]
    Other(#[from] anyhow::Error),
}

pub(crate) fn get_piece_by_hash(
    piece_index_hash: PieceIndexHash,
    piece_cache: &ParityDbStore<
        subspace_networking::libp2p::kad::record::Key,
        subspace_core_primitives::Piece,
    >,
    weak_readers_and_pieces: &std::sync::Weak<parking_lot::Mutex<Option<ReadersAndPieces>>>,
    piece_memory_cache: &PieceMemoryCache,
) -> impl std::future::Future<Output = Option<PieceByHashResponse>> + Send + Sync + 'static {
    use futures::future::{ready, Either};
    use tracing::debug;

    if let Some(piece) = piece_memory_cache.get_piece(&piece_index_hash) {
        return Either::Left(ready(Some(PieceByHashResponse { piece: Some(piece) })));
    }

    if let Some(piece) = piece_cache.get(&piece_index_hash.to_multihash().into()) {
        return Either::Left(ready(Some(PieceByHashResponse { piece: Some(piece) })));
    }

    let weak_readers_and_pieces = weak_readers_and_pieces.clone();

    debug!(?piece_index_hash, "No piece in the cache. Trying archival storage...");

    let read_piece_fut = {
        let readers_and_pieces = match weak_readers_and_pieces.upgrade() {
            Some(readers_and_pieces) => readers_and_pieces,
            None => {
                debug!("A readers and pieces are already dropped");
                return Either::Left(ready(None));
            }
        };
        let readers_and_pieces = readers_and_pieces.lock();
        let readers_and_pieces = match readers_and_pieces.as_ref() {
            Some(readers_and_pieces) => readers_and_pieces,
            None => {
                debug!(?piece_index_hash, "Readers and pieces are not initialized yet");
                return Either::Left(ready(None));
            }
        };

        match readers_and_pieces.read_piece(&piece_index_hash) {
            Some(fut) => fut.instrument(tracing::Span::current()),
            None => return Either::Left(ready(None)),
        }
    };

    Either::Right(read_piece_fut.map(|piece| Some(PieceByHashResponse { piece })))
}

const SEGMENT_COMMITMENTS_CACHE_SIZE: NonZeroUsize =
    NonZeroUsize::new(1_000_000).expect("Not zero; qed");

fn create_readers_and_pieces(single_disk_plots: &[SingleDiskPlot]) -> ReadersAndPieces {
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
}

#[allow(clippy::too_many_arguments)]
fn handler_on_sector_plotted(
    sector_offset: usize,
    plotted_sector: &subspace_farmer_components::plotting::PlottedSector,
    plotting_permit: Arc<impl Send + Sync + 'static>,
    plot_offset: usize,
    node: &subspace_networking::Node,
    readers_and_pieces: Arc<parking_lot::Mutex<Option<ReadersAndPieces>>>,
    mut dropped_receiver: tokio::sync::broadcast::Receiver<()>,
    node_name: &str,
) {
    let sector_index = plotted_sector.sector_index;

    let new_pieces = {
        let mut readers_and_pieces = readers_and_pieces.lock();
        let readers_and_pieces =
            readers_and_pieces.as_mut().expect("Initial value was populated above; qed");

        let new_pieces = plotted_sector
            .piece_indexes
            .iter()
            .filter(|&&piece_index| {
                // Skip pieces that are already plotted and thus were announced
                // before
                !readers_and_pieces.contains_piece(&PieceIndexHash::from_index(piece_index))
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

    // TODO: Skip those that were already announced (because they cached)
    let publish_fut = async move {
        new_pieces
            .into_iter()
            .map(|piece_index| {
                subspace_networking::utils::pieces::announce_single_piece_index_with_backoff(
                    piece_index,
                    &node,
                )
            })
            .collect::<FuturesUnordered<_>>()
            .map(drop)
            .collect::<Vec<()>>()
            .await;

        tracing::info!(sector_offset, sector_index, "Sector publishing was successful.");

        // Release only after publishing is finished
        drop(plotting_permit);
    };

    let _ = utils::task_spawn(
        format!("subspace-sdk-farmer-{node_name}-piece-publishing"),
        async move {
            use futures::future::{select, Either};

            let result = select(Box::pin(publish_fut), Box::pin(dropped_receiver.recv())).await;
            if !matches!(result, Either::Right(_)) {
                tracing::debug!("Piece publishing was cancelled due to shutdown.");
            }
        },
    );
}

impl Config {
    /// Open and start farmer
    pub async fn build(
        self,
        reward_address: PublicKey,
        node: &Node,
        plots: &[PlotDescription],
        cache: CacheDescription,
    ) -> Result<Farmer, BuildError> {
        if plots.is_empty() {
            return Err(BuildError::NoPlotsSupplied);
        }

        let Self {
            max_concurrent_plots,
            piece_cache_size: PieceCacheSize(piece_cache_size),
            provided_keys_limit: ProvidedKeysLimit(provided_keys_limit),
        } = self;

        let piece_cache_size = NonZeroUsize::new(
            piece_cache_size.as_u64() as usize / subspace_core_primitives::Piece::SIZE,
        )
        .ok_or_else(|| anyhow::anyhow!("Piece cache size shouldn't be zero"))?;

        let mut single_disk_plots = Vec::with_capacity(plots.len());
        let mut plot_info = HashMap::with_capacity(plots.len());

        let concurrent_plotting_semaphore =
            Arc::new(tokio::sync::Semaphore::new(max_concurrent_plots.get()));

        let base_path = cache.directory;
        let readers_and_pieces = Arc::clone(&node.dsn.farmer_readers_and_pieces);

        let node_name = node.name.clone();

        let piece_cache_db_path = base_path.join("piece_cache_db");

        let (piece_store, piece_cache, farmer_provider_storage) = {
            let provider_db_path = base_path.join("providers_db");
            // TODO: Remove this migration code in the future
            {
                let provider_cache_db_path = base_path.join("provider_cache_db");
                if provider_cache_db_path.exists() {
                    tokio::fs::rename(&provider_cache_db_path, &provider_db_path)
                        .await
                        .context("Migration of provider db failed")?;
                }
            }

            tracing::info!(
                db_path = ?provider_db_path,
                keys_limit = ?provided_keys_limit,
                "Initializing provider storage..."
            );

            let peer_id = node.dsn.node.id();

            let db_provider_storage =
                ParityDbProviderStorage::new(&provider_db_path, provided_keys_limit, peer_id)
                    .context("Failed to create parity db provider storage")?;

            tracing::info!(
                db_path = ?piece_cache_db_path,
                size = ?piece_cache_size,
                "Initializing piece cache..."
            );

            let piece_store = ParityDbStore::new(&piece_cache_db_path)
                .context("Failed to create parity db piece store")?;

            let piece_cache = FarmerPieceCache::new(piece_store.clone(), piece_cache_size, peer_id);

            tracing::info!(
                current_size = ?piece_cache.size(),
                "Piece cache initialized successfully"
            );
            let farmer_provider_storage = FarmerProviderStorage::new(
                peer_id,
                Arc::clone(&readers_and_pieces),
                db_provider_storage,
                piece_cache.clone(),
            );

            (piece_store, Arc::new(Mutex::new(piece_cache)), farmer_provider_storage)
        };

        let mut drop_at_exit = DropCollection::new();
        let kzg = kzg::Kzg::new(kzg::embedded_kzg_settings());

        let piece_provider = subspace_networking::utils::piece_provider::PieceProvider::new(
            node.dsn.node.clone(),
            Some(SegmentCommitmentPieceValidator::new(
                node.dsn.node.clone(),
                node.rpc_handle.clone(),
                kzg.clone(),
                // TODO: Consider introducing and using global in-memory segment commitments cache
                parking_lot::Mutex::new(lru::LruCache::new(SEGMENT_COMMITMENTS_CACHE_SIZE)),
            )),
        );
        let piece_getter = Arc::new(FarmerPieceGetter::new(
            NodePieceGetter::new(DsnPieceGetter::new(piece_provider), node.dsn.piece_cache.clone()),
            Arc::clone(&piece_cache),
            node.dsn.node.clone(),
        ));

        for (disk_farm_idx, description) in plots.iter().enumerate() {
            let (plot, single_disk_plot) = Plot::new(
                disk_farm_idx,
                reward_address,
                node,
                Arc::clone(&piece_getter),
                Arc::clone(&concurrent_plotting_semaphore),
                description,
                kzg.clone(),
            )
            .await?;
            plot_info.insert(plot.directory.clone(), plot);
            single_disk_plots.push(single_disk_plot);
        }

        readers_and_pieces.lock().replace(create_readers_and_pieces(&single_disk_plots));

        for (plot_offset, single_disk_plot) in single_disk_plots.iter().enumerate() {
            let readers_and_pieces = Arc::clone(&readers_and_pieces);

            // We are not going to send anything here, but dropping of sender on dropping of
            // corresponding `SingleDiskPlot` will allow us to stop background tasks.
            let (dropped_sender, _dropped_receiver) = tokio::sync::broadcast::channel::<()>(1);
            drop_at_exit.defer({
                let dropped_sender = dropped_sender.clone();
                move || drop(dropped_sender.send(()))
            });

            let node = node.dsn.node.clone();
            let node_name = node_name.clone();
            // Collect newly plotted pieces
            // TODO: Once we have replotting, this will have to be updated
            let handler_id = single_disk_plot.on_sector_plotted(Arc::new(
                move |(sector_offset, plotted_sector, plotting_permit)| {
                    handler_on_sector_plotted(
                        *sector_offset,
                        plotted_sector,
                        Arc::clone(plotting_permit),
                        plot_offset,
                        &node,
                        readers_and_pieces.clone(),
                        dropped_sender.subscribe(),
                        &node_name,
                    )
                },
            ));

            drop_at_exit.push(handler_id);
        }

        let mut single_disk_plots_stream =
            single_disk_plots.into_iter().map(SingleDiskPlot::run).collect::<FuturesUnordered<_>>();

        let (drop_sender, drop_receiver) = oneshot::channel::<()>();
        let (result_sender, result_receiver) = oneshot::channel::<_>();

        utils::task_spawn_blocking(format!("subspace-sdk-farmer-{node_name}-plots-driver"), {
            let handle = tokio::runtime::Handle::current();
            let is_node_closed = {
                let stop_sender = node.stop_sender.clone();
                move || stop_sender.is_closed()
            };

            move || {
                use future::Either::*;

                let result = match handle
                    .block_on(future::select(single_disk_plots_stream.next(), drop_receiver))
                {
                    Left((_, _)) if is_node_closed() => Ok(()),
                    Left((maybe_result, _)) =>
                        maybe_result.expect("there is always at least one plot"),
                    Right((_, _)) => Ok(()),
                };
                let _ = result_sender.send(result);
            }
        });

        node.dsn.farmer_piece_store.lock().await.replace(piece_store);
        node.dsn.farmer_provider_storage.swap(Some(farmer_provider_storage));

        drop_at_exit.push(
            crate::dsn::start_announcements_processor(
                node.dsn.node.clone(),
                Arc::clone(&piece_cache),
                Arc::downgrade(&readers_and_pieces),
                &node.name,
            )
            .context("Failed to start announcement processor")?,
        );

        drop_at_exit.defer({
            let provider_storage = node.dsn.farmer_provider_storage.clone();
            move || {
                let _ = drop_sender.send(());
                provider_storage.swap(None);
            }
        });

        let mut async_drop = AsyncDropFutures::new();

        async_drop.push({
            let piece_store = Arc::clone(&node.dsn.farmer_piece_store);
            async move {
                piece_store.lock().await.take();
            }
        });

        async_drop.push(async move {
            const PIECE_STORE_POLL: Duration = Duration::from_millis(100);

            // HACK: Poll on piece store creation just to be sure
            loop {
                let result = ParityDbStore::<
                    subspace_networking::libp2p::kad::record::Key,
                    subspace_core_primitives::Piece,
                >::new(&piece_cache_db_path);

                match result.map(drop) {
                    // If parity db is still locked wait on it
                    Err(parity_db::Error::Locked(_)) => tokio::time::sleep(PIECE_STORE_POLL).await,
                    _ => break,
                }
            }
        });

        tracing::debug!("Started farmer");

        Ok(Farmer {
            reward_address,
            plot_info,
            result_receiver: Some(result_receiver),
            node_name,
            base_path,
            _drop_at_exit: drop_at_exit,
            _async_drop: Some(async_drop),
        })
    }
}

/// Farmer structure
#[derive(Derivative)]
#[derivative(Debug)]
#[must_use = "Farmer should be closed"]
pub struct Farmer {
    reward_address: PublicKey,
    plot_info: HashMap<PathBuf, Plot>,
    result_receiver: Option<oneshot::Receiver<anyhow::Result<()>>>,
    base_path: PathBuf,
    node_name: String,
    _drop_at_exit: DropCollection,
    _async_drop: Option<AsyncDropFutures>,
}

static_assertions::assert_impl_all!(Farmer: Send, Sync);
static_assertions::assert_impl_all!(Plot: Send, Sync);

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
    progress:
        watch::Receiver<Option<(usize, PlottedSector, Arc<tokio::sync::OwnedSemaphorePermit>)>>,
    solutions: watch::Receiver<Option<SolutionResponse>>,
    initial_plotting_progress: Arc<Mutex<InitialPlottingProgress>>,
    allocated_space: u64,
    _drop_at_exit: DropCollection,
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
    async fn new(
        disk_farm_idx: usize,
        reward_address: PublicKey,
        node: &Node,
        piece_getter: impl subspace_farmer_components::plotting::PieceGetter + Send + 'static,
        concurrent_plotting_semaphore: Arc<tokio::sync::Semaphore>,
        description: &PlotDescription,
        kzg: kzg::Kzg,
    ) -> Result<(Self, SingleDiskPlot), BuildError> {
        let directory = description.directory.clone();
        let allocated_space = description.space_pledged.as_u64();
        let description = SingleDiskPlotOptions {
            allocated_space,
            directory: directory.clone(),
            reward_address: *reward_address,
            node_client: node.rpc_handle.clone(),
            kzg,
            piece_getter,
            concurrent_plotting_semaphore,
            piece_memory_cache: node.dsn.piece_memory_cache.clone(),
        };
        let single_disk_plot = SingleDiskPlot::new(description, disk_farm_idx).await?;
        let mut drop_at_exit = DropCollection::new();

        let progress = {
            let (sender, receiver) = watch::channel::<Option<_>>(None);
            drop_at_exit.push(single_disk_plot.on_sector_plotted(Arc::new(move |sector| {
                let _ = sender.send(Some(sector.clone()));
            })));
            receiver
        };
        let solutions = {
            let (sender, receiver) = watch::channel::<Option<_>>(None);
            drop_at_exit.push(single_disk_plot.on_solution(Arc::new(move |solution| {
                let _ = sender.send(Some(solution.clone()));
            })));
            receiver
        };

        Ok((
            Self {
                directory: directory.clone(),
                allocated_space,
                progress,
                solutions,
                initial_plotting_progress: Arc::new(Mutex::new(InitialPlottingProgress {
                    starting_sector: single_disk_plot.plotted_sectors_count(),
                    current_sector: single_disk_plot.plotted_sectors_count(),
                    total_sectors: allocated_space / subspace_core_primitives::PLOT_SECTOR_SIZE,
                })),
                _drop_at_exit: drop_at_exit,
            },
            single_disk_plot,
        ))
    }

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
                        let plotting_progress = *guard;
                        guard.current_sector += 1;
                        Some(plotting_progress)
                    }
                }
            })
            .take_while(|InitialPlottingProgress { current_sector, total_sectors, .. }| {
                futures::future::ready(current_sector < total_sectors)
            });
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
    pub async fn close(mut self) -> anyhow::Result<()> {
        let result_receiver = self.result_receiver.take().expect("Handle is always there");
        let async_drop = self._async_drop.take().expect("Always set").async_drop();

        drop(self);
        async_drop.await;

        result_receiver.await?
    }
}

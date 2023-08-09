//! This crate is related to abstract farmer implementation

#![warn(
    missing_docs,
    clippy::dbg_macro,
    clippy::unwrap_used,
    clippy::disallowed_types,
    unused_features
)]
#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![feature(const_option)]

mod combined_piece_getter;

use std::collections::HashMap;
use std::io;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{anyhow, Context};
pub use builder::{Builder, Config};
use derivative::Derivative;
use futures::prelude::*;
use futures::stream::FuturesUnordered;
use sdk_dsn::FarmerProviderStorage;
use sdk_traits::Node;
use sdk_utils::{ByteSize, Destructors, PublicKey};
use serde::{Deserialize, Serialize};
use subspace_core_primitives::crypto::kzg;
use subspace_core_primitives::{PieceIndexHash, Record, SectorIndex};
use subspace_erasure_coding::ErasureCoding;
use subspace_farmer::piece_cache::PieceCache;
use subspace_farmer::single_disk_plot::{
    SingleDiskPlot, SingleDiskPlotError, SingleDiskPlotId, SingleDiskPlotInfo,
    SingleDiskPlotOptions, SingleDiskPlotSummary,
};
use subspace_farmer::utils::archival_storage_pieces::ArchivalStoragePieces;
use subspace_farmer::utils::piece_validator::SegmentCommitmentPieceValidator;
use subspace_farmer::utils::readers_and_pieces::ReadersAndPieces;
use subspace_farmer_components::plotting::PlottedSector;
use subspace_networking::libp2p::kad::RecordKey;
use subspace_networking::utils::multihash::ToMultihash;
use subspace_rpc_primitives::{FarmerAppInfo, SolutionResponse};
use tokio::sync::{mpsc, oneshot, watch, Mutex};
use tracing::{debug, error, warn};
use tracing_futures::Instrument;

use crate::combined_piece_getter::CombinedPieceGetter;

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

impl PlotDescription {
    /// Construct Plot description
    pub fn new(directory: impl Into<PathBuf>, space_pledged: ByteSize) -> Self {
        Self { directory: directory.into(), space_pledged }
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
    use sdk_traits::Node;
    use sdk_utils::{ByteSize, PublicKey};
    use serde::{Deserialize, Serialize};

    use super::{BuildError, CacheDescription};
    use crate::{Farmer, PlotDescription};

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
        #[serde(default, skip_serializing_if = "sdk_utils::is_default")]
        pub max_concurrent_plots: MaxConcurrentPlots,
        /// Number of plots that can be plotted concurrently, impacts RAM usage.
        #[builder(default, setter(into))]
        #[serde(default, skip_serializing_if = "sdk_utils::is_default")]
        pub provided_keys_limit: ProvidedKeysLimit,
        /// Maximum number of pieces in single sector
        #[builder(default)]
        pub max_pieces_in_sector: Option<u16>,
    }

    impl Builder {
        /// Get configuration for saving on disk
        pub fn configuration(&self) -> Config {
            self._build().expect("Build is infallible")
        }

        /// Open and start farmer
        pub async fn build<N: Node>(
            self,
            reward_address: PublicKey,
            node: &N,
            plots: &[PlotDescription],
            cache: CacheDescription,
        ) -> Result<Farmer<N::Table>, BuildError> {
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

#[async_trait::async_trait]
impl<T: subspace_proof_of_space::Table> sdk_traits::Farmer for Farmer<T> {
    type Table = T;

    async fn get_piece_by_hash(
        piece_index_hash: PieceIndexHash,
        piece_cache: &PieceCache,
        weak_readers_and_pieces: &std::sync::Weak<parking_lot::Mutex<Option<ReadersAndPieces>>>,
    ) -> Option<subspace_core_primitives::Piece> {
        use tracing::debug;

        if let Some(piece) =
            piece_cache.get_piece(RecordKey::from(piece_index_hash.to_multihash())).await
        {
            return Some(piece);
        }

        let weak_readers_and_pieces = weak_readers_and_pieces.clone();

        debug!(?piece_index_hash, "No piece in the cache. Trying archival storage...");

        let readers_and_pieces = match weak_readers_and_pieces.upgrade() {
            Some(readers_and_pieces) => readers_and_pieces,
            None => {
                debug!("A readers and pieces are already dropped");
                return None;
            }
        };
        let read_piece = match readers_and_pieces.lock().as_ref() {
            Some(readers_and_pieces) => readers_and_pieces.read_piece(&piece_index_hash),
            None => {
                debug!(?piece_index_hash, "Readers and pieces are not initialized yet");
                return None;
            }
        };

        match read_piece {
            Some(fut) => fut.in_current_span().await,
            None => None,
        }
    }
}

const SEGMENT_COMMITMENTS_CACHE_SIZE: NonZeroUsize =
    NonZeroUsize::new(1_000_000).expect("Not zero; qed");

fn create_readers_and_pieces(
    single_disk_plots: &[SingleDiskPlot],
    archival_storage_pieces: ArchivalStoragePieces,
) -> Result<ReadersAndPieces, BuildError> {
    // Store piece readers so we can reference them later
    let readers = single_disk_plots.iter().map(SingleDiskPlot::piece_reader).collect();
    let mut readers_and_pieces = ReadersAndPieces::new(readers, archival_storage_pieces);

    tracing::debug!("Collecting already plotted pieces");

    // Collect already plotted pieces
    single_disk_plots.iter().enumerate().try_for_each(|(disk_farm_index, single_disk_plot)| {
        let disk_farm_index = disk_farm_index.try_into().map_err(|_error| {
            anyhow!(
                "More than 256 plots are not supported, consider running multiple farmer instances"
            )
        })?;

        (0 as SectorIndex..).zip(single_disk_plot.plotted_sectors()).for_each(
            |(sector_index, plotted_sector_result)| match plotted_sector_result {
                Ok(plotted_sector) => {
                    readers_and_pieces.add_sector(disk_farm_index, &plotted_sector);
                }
                Err(error) => {
                    error!(
                        %error,
                        %disk_farm_index,
                        %sector_index,
                        "Failed reading plotted sector on startup, skipping"
                    );
                }
            },
        );

        Ok::<_, anyhow::Error>(())
    })?;
    tracing::debug!("Finished collecting already plotted pieces");

    Ok(readers_and_pieces)
}

#[allow(clippy::too_many_arguments)]
fn handler_on_sector_plotted(
    plotted_sector: &PlottedSector,
    maybe_old_plotted_sector: &Option<PlottedSector>,
    disk_farm_index: usize,
    readers_and_pieces: Arc<parking_lot::Mutex<Option<ReadersAndPieces>>>,
) {
    let disk_farm_index = disk_farm_index
        .try_into()
        .expect("More than 256 plots are not supported, this is checked above already; qed");

    {
        let mut readers_and_pieces = readers_and_pieces.lock();
        let readers_and_pieces =
            readers_and_pieces.as_mut().expect("Initial value was populated before; qed");

        if let Some(old_plotted_sector) = maybe_old_plotted_sector {
            readers_and_pieces.delete_sector(disk_farm_index, old_plotted_sector);
        }
        readers_and_pieces.add_sector(disk_farm_index, plotted_sector);
    }
}

impl Config {
    /// Open and start farmer
    pub async fn build<N: Node, T: subspace_proof_of_space::Table>(
        self,
        reward_address: PublicKey,
        node: &N,
        plots: &[PlotDescription],
        cache: CacheDescription,
    ) -> Result<Farmer<T>, BuildError> {
        if plots.is_empty() {
            return Err(BuildError::NoPlotsSupplied);
        }

        let mut destructors = Destructors::new();

        let Self { max_concurrent_plots: _, provided_keys_limit: _, max_pieces_in_sector } = self;

        let mut single_disk_plots = Vec::with_capacity(plots.len());
        let mut plot_info = HashMap::with_capacity(plots.len());

        let base_path = cache.directory;
        let readers_and_pieces = Arc::clone(&node.dsn().farmer_readers_and_pieces);

        let node_name = node.name().to_owned();

        let peer_id = node.dsn().node.id();

        let (farmer_piece_cache, farmer_piece_cache_worker) =
            PieceCache::new(node.rpc().clone(), peer_id);

        let farmer_provider_storage =
            FarmerProviderStorage::new(peer_id, farmer_piece_cache.clone());

        let kzg = kzg::Kzg::new(kzg::embedded_kzg_settings());
        let erasure_coding = ErasureCoding::new(
            NonZeroUsize::new(Record::NUM_S_BUCKETS.next_power_of_two().ilog2() as usize).expect(
                "Number of buckets >= 1, therefore next power of 2 >= 2, therefore ilog2 >= 1",
            ),
        )
        .map_err(|error| anyhow::anyhow!("Failed to create erasure coding for plot: {error}"))?;

        let piece_provider = subspace_networking::utils::piece_provider::PieceProvider::new(
            node.dsn().node.clone(),
            Some(SegmentCommitmentPieceValidator::new(
                node.dsn().node.clone(),
                node.rpc().clone(),
                kzg.clone(),
                // TODO: Consider introducing and using global in-memory segment commitments cache
                parking_lot::Mutex::new(lru::LruCache::new(SEGMENT_COMMITMENTS_CACHE_SIZE)),
            )),
        );
        let piece_getter = Arc::new(CombinedPieceGetter::new(
            node.dsn().node.clone(),
            piece_provider,
            farmer_piece_cache.clone(),
            node.dsn().node_piece_cache.clone(),
            node.dsn().farmer_archival_storage_info.clone(),
        ));

        let (piece_cache_worker_drop_sender, piece_cache_worker_drop_receiver) =
            oneshot::channel::<()>();
        let farmer_piece_cache_worker_join_handle = sdk_utils::task_spawn_blocking(
            format!("subspace-sdk-farmer-{node_name}-pieces-cache-worker"),
            {
                let handle = tokio::runtime::Handle::current();
                let piece_getter = piece_getter.clone();

                move || {
                    handle.block_on(future::select(
                        Box::pin({
                            let piece_getter = piece_getter.clone();
                            farmer_piece_cache_worker.run(piece_getter)
                        }),
                        piece_cache_worker_drop_receiver,
                    ));
                }
            },
        );

        destructors.add_async_destructor({
            async move {
                let _ = piece_cache_worker_drop_sender.send(());
                farmer_piece_cache_worker_join_handle.await.expect(
                    "awaiting worker should not fail except panic by the worker itself; qed",
                );
            }
        })?;

        let farmer_app_info = subspace_farmer::NodeClient::farmer_app_info(node.rpc())
            .await
            .expect("Node is always reachable");

        let max_pieces_in_sector = match max_pieces_in_sector {
            Some(m) => m,
            None => farmer_app_info.protocol_info.max_pieces_in_sector,
        };

        for (disk_farm_idx, description) in plots.iter().enumerate() {
            let (plot, single_disk_plot) = Plot::new(PlotOptions {
                disk_farm_idx,
                reward_address,
                node,
                max_pieces_in_sector,
                piece_getter: Arc::clone(&piece_getter),
                description,
                kzg: kzg.clone(),
                erasure_coding: erasure_coding.clone(),
            })
            .await?;
            plot_info.insert(plot.directory.clone(), plot);
            single_disk_plots.push(single_disk_plot);
        }

        *node.dsn().farmer_piece_cache.write().await = Some(farmer_piece_cache.clone());
        destructors.add_async_destructor({
            let piece_cache = Arc::clone(&node.dsn().farmer_piece_cache);
            async move {
                piece_cache.write().await.take();
            }
        })?;

        node.dsn().farmer_provider_storage.swap(Some(farmer_provider_storage));
        destructors.add_sync_destructor({
            let provider_storage = node.dsn().farmer_provider_storage.clone();
            move || {
                provider_storage.swap(None);
            }
        })?;

        farmer_piece_cache
            .replace_backing_caches(
                single_disk_plots
                    .iter()
                    .map(|single_disk_plot| single_disk_plot.piece_cache())
                    .collect(),
            )
            .await;
        drop(farmer_piece_cache);

        readers_and_pieces.lock().replace(create_readers_and_pieces(
            &single_disk_plots,
            node.dsn().farmer_archival_storage_pieces.clone(),
        )?);
        destructors.add_sync_destructor({
            let farmer_reader_and_pieces = node.dsn().farmer_readers_and_pieces.clone();
            move || {
                farmer_reader_and_pieces.lock().take();
            }
        })?;

        let mut sector_plotting_handler_ids = vec![];
        for (disk_farm_index, single_disk_plot) in single_disk_plots.iter().enumerate() {
            let readers_and_pieces = Arc::clone(&readers_and_pieces);
            let span = tracing::info_span!("farm", %disk_farm_index);

            // Collect newly plotted pieces
            // TODO: Once we have replotting, this will have to be updated
            sector_plotting_handler_ids.push(single_disk_plot.on_sector_plotted(Arc::new(
                move |(plotted_sector, maybe_old_plotted_sector)| {
                    let _span_guard = span.enter();
                    handler_on_sector_plotted(
                        plotted_sector,
                        maybe_old_plotted_sector,
                        disk_farm_index,
                        readers_and_pieces.clone(),
                    )
                },
            )));
        }

        let mut single_disk_plots_stream =
            single_disk_plots.into_iter().map(SingleDiskPlot::run).collect::<FuturesUnordered<_>>();

        let (plot_driver_drop_sender, mut plot_driver_drop_receiver) = oneshot::channel::<()>();
        let (plot_driver_result_sender, plot_driver_result_receiver) =
            mpsc::channel::<_>(u8::MAX as usize + 1);

        let plot_driver_join_handle = sdk_utils::task_spawn_blocking(
            format!("subspace-sdk-farmer-{node_name}-plots-driver"),
            {
                let handle = tokio::runtime::Handle::current();

                move || {
                    use future::Either::*;

                    loop {
                        let result = handle.block_on(future::select(
                            single_disk_plots_stream.next(),
                            &mut plot_driver_drop_receiver,
                        ));

                        match result {
                            Left((maybe_result, _)) => {
                                let result = plot_driver_result_sender.try_send(
                                    maybe_result
                                        .expect("We have at least one plot")
                                        .context("Farmer exited with error"),
                                );

                                // Receiver is closed which would mean we are shutting down
                                if result.is_err() {
                                    break;
                                }
                            }
                            Right((_, _)) => {
                                let _ = plot_driver_result_sender.try_send(Ok(()));
                                break;
                            }
                        };
                    }
                }
            },
        );

        destructors.add_async_destructor({
            async move {
                let _ = plot_driver_drop_sender.send(());
                plot_driver_join_handle.await.expect("joining should not fail; qed");
            }
        })?;

        for handler_id in sector_plotting_handler_ids.drain(..) {
            destructors.add_items_to_drop(handler_id)?;
        }

        // TODO: check for piece cache to exit
        // async_drop.push(async move {
        //     const PIECE_STORE_POLL: Duration = Duration::from_millis(100);

        //     // HACK: Poll on piece store creation just to be sure
        //     loop {
        //         let result = ParityDbStore::<
        //             subspace_networking::libp2p::kad::record::Key,
        //             subspace_core_primitives::Piece,
        //         >::new(&piece_cache_db_path);

        //         match result.map(drop) {
        //             // If parity db is still locked wait on it
        //             Err(parity_db::Error::Locked(_)) =>
        // tokio::time::sleep(PIECE_STORE_POLL).await,             _ => break,
        //         }
        //     }
        // });

        tracing::debug!("Started farmer");

        Ok(Farmer {
            reward_address,
            plot_info,
            result_receiver: Some(plot_driver_result_receiver),
            node_name,
            base_path,
            app_info: subspace_farmer::NodeClient::farmer_app_info(node.rpc())
                .await
                .expect("Node is always reachable"),
            _destructors: destructors,
        })
    }
}

/// Farmer structure
#[derive(Derivative)]
#[derivative(Debug)]
#[must_use = "Farmer should be closed"]
pub struct Farmer<T: subspace_proof_of_space::Table> {
    reward_address: PublicKey,
    plot_info: HashMap<PathBuf, Plot<T>>,
    result_receiver: Option<mpsc::Receiver<anyhow::Result<()>>>,
    base_path: PathBuf,
    node_name: String,
    app_info: FarmerAppInfo,
    _destructors: Destructors,
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
    /// How much space in bytes is allocated for this plot
    pub allocated_space: ByteSize,
    /// How many pieces are in sector
    pub pieces_in_sector: u16,
}

impl From<SingleDiskPlotInfo> for PlotInfo {
    fn from(info: SingleDiskPlotInfo) -> Self {
        let SingleDiskPlotInfo::V0 {
            id,
            genesis_hash,
            public_key,
            allocated_space,
            pieces_in_sector,
        } = info;
        Self {
            id,
            genesis_hash,
            public_key: PublicKey(public_key),
            allocated_space: ByteSize::b(allocated_space),
            pieces_in_sector,
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

/// Progress data received from sender, used to monitor plotting progress
pub type ProgressData = Option<(PlottedSector, Option<PlottedSector>)>;

/// Plot structure
#[derive(Debug)]
pub struct Plot<T: subspace_proof_of_space::Table> {
    directory: PathBuf,
    progress: watch::Receiver<ProgressData>,
    solutions: watch::Receiver<Option<SolutionResponse>>,
    initial_plotting_progress: Arc<Mutex<InitialPlottingProgress>>,
    allocated_space: u64,
    _destructors: Destructors,
    _table: std::marker::PhantomData<T>,
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

struct PlotOptions<'a, PG, N: sdk_traits::Node> {
    pub disk_farm_idx: usize,
    pub reward_address: PublicKey,
    pub node: &'a N,
    pub piece_getter: PG,
    pub description: &'a PlotDescription,
    pub kzg: kzg::Kzg,
    pub erasure_coding: ErasureCoding,
    pub max_pieces_in_sector: u16,
}

impl<T: subspace_proof_of_space::Table> Plot<T> {
    async fn new(
        PlotOptions {
            disk_farm_idx,
            reward_address,
            node,
            piece_getter,
            description,
            kzg,
            erasure_coding,
            max_pieces_in_sector,
        }: PlotOptions<
            '_,
            impl subspace_farmer_components::plotting::PieceGetter + Send + 'static,
            impl sdk_traits::Node,
        >,
    ) -> Result<(Self, SingleDiskPlot), BuildError> {
        let directory = description.directory.clone();
        let allocated_space = description.space_pledged.as_u64();
        let farmer_app_info = subspace_farmer::NodeClient::farmer_app_info(node.rpc())
            .await
            .expect("Node is always reachable");
        let description = SingleDiskPlotOptions {
            allocated_space,
            directory: directory.clone(),
            farmer_app_info,
            max_pieces_in_sector,
            reward_address: *reward_address,
            node_client: node.rpc().clone(),
            kzg,
            erasure_coding,
            piece_getter,
        };
        let single_disk_plot = SingleDiskPlot::new::<_, _, T>(description, disk_farm_idx).await?;
        let mut destructors = Destructors::new();

        let progress = {
            let (sender, receiver) = watch::channel::<Option<_>>(None);
            destructors.add_items_to_drop(single_disk_plot.on_sector_plotted(Arc::new(
                move |sector| {
                    let _ = sender.send(Some(sector.clone()));
                },
            )))?;
            receiver
        };
        let solutions = {
            let (sender, receiver) = watch::channel::<Option<_>>(None);
            destructors.add_items_to_drop(single_disk_plot.on_solution(Arc::new(
                move |solution| {
                    let _ = sender.send(Some(solution.clone()));
                },
            )))?;
            receiver
        };

        Ok((
            Self {
                directory: directory.clone(),
                allocated_space,
                progress,
                solutions,
                initial_plotting_progress: Arc::new(Mutex::new(InitialPlottingProgress {
                    starting_sector: u64::try_from(single_disk_plot.plotted_sectors_count())
                        .expect("Sector count is less than u64::MAX"),
                    current_sector: u64::try_from(single_disk_plot.plotted_sectors_count())
                        .expect("Sector count is less than u64::MAX"),
                    total_sectors: allocated_space
                        / subspace_farmer_components::sector::sector_size(max_pieces_in_sector)
                            as u64,
                })),
                _destructors: destructors,
                _table: Default::default(),
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

impl<T: subspace_proof_of_space::Table> Farmer<T> {
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
            sector_size: subspace_farmer_components::sector::sector_size(
                self.app_info.protocol_info.max_pieces_in_sector,
            ) as _,
        })
    }

    /// Iterate over plots
    pub async fn iter_plots(&'_ self) -> impl Iterator<Item = &'_ Plot<T>> + '_ {
        self.plot_info.values()
    }

    /// Stops farming, closes plots, and sends signal to the node
    pub async fn close(mut self) -> anyhow::Result<()> {
        let mut result_receiver = self.result_receiver.take().expect("Handle is always there");
        result_receiver.close();
        while let Some(msg) = result_receiver.recv().await {
            msg?;
        }
        self._destructors.run().await
    }
}

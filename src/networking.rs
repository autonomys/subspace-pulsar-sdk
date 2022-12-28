use subspace_core_primitives::{Piece, PieceIndexHash, SectorIndex};
use subspace_farmer::single_disk_plot::piece_reader::PieceReader;
use subspace_farmer::single_disk_plot::SingleDiskPlot;
use subspace_networking::{LimitedSizeRecordStorageWrapper, ParityDbRecordStorage};
use subspace_service::piece_cache::PieceCache;

#[derive(Debug, Copy, Clone)]
pub struct PieceDetails {
    pub plot_offset: usize,
    pub sector_index: SectorIndex,
    pub piece_offset: u64,
}

#[derive(Debug)]
pub struct ReadersAndPieces {
    readers: Vec<PieceReader>,
    pieces: std::collections::HashMap<PieceIndexHash, PieceDetails>,
    handle: tokio::runtime::Handle,
}

impl ReadersAndPieces {
    pub async fn new(single_disk_plots: &[SingleDiskPlot]) -> Self {
        // Store piece readers so we can reference them later
        let readers = single_disk_plots.iter().map(SingleDiskPlot::piece_reader).collect();

        tracing::debug!("Collecting already plotted pieces");

        // Collect already plotted pieces
        let pieces = single_disk_plots
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

        let handle = tokio::runtime::Handle::current();
        Self { readers, pieces, handle }
    }

    pub fn get_piece(&self, key: &PieceIndexHash) -> Option<Piece> {
        let Some(piece_details) = self.pieces.get(key).copied() else {
            tracing::trace!(?key, "Piece is not stored in any of the local plots");
            return None
        };
        let mut reader = self
            .readers
            .get(piece_details.plot_offset)
            .cloned()
            .expect("Offsets strictly correspond to existing plots; qed");

        let handle = &self.handle;
        tokio::task::block_in_place(move || {
            handle
                .block_on(reader.read_piece(piece_details.sector_index, piece_details.piece_offset))
        })
    }
}

impl Extend<(PieceIndexHash, PieceDetails)> for ReadersAndPieces {
    fn extend<T: IntoIterator<Item = (PieceIndexHash, PieceDetails)>>(&mut self, iter: T) {
        self.pieces.extend(iter)
    }
}

#[derive(Debug)]
pub struct MaybeRecordStorage<S> {
    inner: std::sync::Arc<std::sync::Mutex<Option<S>>>,
}

impl<S> Clone for MaybeRecordStorage<S> {
    fn clone(&self) -> Self {
        Self { inner: std::sync::Arc::clone(&self.inner) }
    }
}

impl<S> MaybeRecordStorage<S> {
    pub fn none() -> Self {
        Self { inner: std::sync::Arc::new(std::sync::Mutex::new(None)) }
    }

    pub fn swap(&self, value: S) {
        *self.inner.lock().unwrap() = Some(value);
    }
}

impl<S: subspace_networking::RecordStorage> subspace_networking::RecordStorage
    for MaybeRecordStorage<S>
{
    fn get(
        &'_ self,
        k: &subspace_networking::libp2p::kad::record::Key,
    ) -> Option<std::borrow::Cow<'_, subspace_networking::libp2p::kad::Record>> {
        let lock = self.inner.lock().unwrap();
        let Some(x) = &*lock else { return None };
        x.get(k)
            .map(std::borrow::Cow::into_owned)
            .map(std::borrow::Cow::<'static, subspace_networking::libp2p::kad::Record>::Owned)
    }

    fn put(
        &mut self,
        r: subspace_networking::libp2p::kad::Record,
    ) -> subspace_networking::libp2p::kad::store::Result<()> {
        self.inner.lock().unwrap().as_mut().map(|x| x.put(r)).unwrap_or(Ok(()))
    }

    fn remove(&mut self, k: &subspace_networking::libp2p::kad::record::Key) {
        if let Some(x) = self.inner.lock().unwrap().as_mut() {
            x.remove(k)
        }
    }
}

pub struct AndRecordStorage<A, B> {
    a: A,
    b: B,
}

impl<A, B> AndRecordStorage<A, B> {
    pub fn new(a: A, b: B) -> Self {
        Self { a, b }
    }
}

impl<A: subspace_networking::RecordStorage, B: subspace_networking::RecordStorage>
    subspace_networking::RecordStorage for AndRecordStorage<A, B>
{
    fn get(
        &'_ self,
        k: &subspace_networking::libp2p::kad::record::Key,
    ) -> Option<std::borrow::Cow<'_, subspace_networking::libp2p::kad::Record>> {
        self.a.get(k).or_else(|| self.b.get(k))
    }

    fn put(
        &mut self,
        r: subspace_networking::libp2p::kad::Record,
    ) -> subspace_networking::libp2p::kad::store::Result<()> {
        self.a.put(r.clone())?;
        self.b.put(r)
    }

    fn remove(&mut self, k: &subspace_networking::libp2p::kad::record::Key) {
        self.a.remove(k);
        self.b.remove(k)
    }
}

pub type FarmerRecordStorage = LimitedSizeRecordStorageWrapper<ParityDbRecordStorage>;
pub type NodeRecordStorage<C> = PieceCache<C>;
pub type RecordStorage<C> =
    AndRecordStorage<MaybeRecordStorage<FarmerRecordStorage>, NodeRecordStorage<C>>;

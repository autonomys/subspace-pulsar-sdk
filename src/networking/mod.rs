pub(crate) mod farmer_piece_storage;
pub(crate) mod farmer_provider_record_processor;
pub(crate) mod farmer_provider_storage;
pub(crate) mod node_provider_storage;

use std::borrow::Cow;
use std::sync::Arc;

use derivative::Derivative;
use either::*;
use parking_lot::Mutex;
use subspace_core_primitives::{Piece, PieceIndexHash, SectorIndex};
use subspace_farmer::single_disk_plot::piece_reader::PieceReader;
use subspace_farmer::single_disk_plot::SingleDiskPlot;
use subspace_networking::libp2p::kad::record::Key;
use subspace_networking::libp2p::kad::ProviderRecord;
use subspace_networking::ParityDbProviderStorage;
use subspace_service::piece_cache::PieceCache;

/// Defines persistent piece storage interface.
pub trait PieceStorage: Sync + Send + 'static {
    /// Check whether key should be inserted into the storage with current
    /// storage size and key-to-peer-id distance.
    fn should_include_in_storage(&self, key: &Key) -> bool;

    /// Add piece to the storage.
    fn add_piece(&mut self, key: Key, piece: Piece);

    /// Get piece from the storage.
    fn get_piece(&self, key: &Key) -> Option<Piece>;
}

#[derive(Debug, Copy, Clone)]
pub struct PieceDetails {
    pub plot_offset: usize,
    pub sector_index: SectorIndex,
    pub piece_offset: u64,
}

#[derive(Debug)]
pub struct ReadersAndPieces {
    pub readers: Vec<PieceReader>,
    pub pieces: std::collections::HashMap<PieceIndexHash, PieceDetails>,
    pub handle: tokio::runtime::Handle,
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

#[derive(Derivative)]
#[derivative(Debug)]
pub struct MaybeProviderStorage<S> {
    #[derivative(Debug = "ignore")]
    inner: Arc<Mutex<Option<S>>>,
}

impl<S> Clone for MaybeProviderStorage<S> {
    fn clone(&self) -> Self {
        Self { inner: Arc::clone(&self.inner) }
    }
}

impl<S> MaybeProviderStorage<S> {
    pub fn none() -> Self {
        Self { inner: Arc::new(Mutex::new(None)) }
    }

    pub fn swap(&self, value: S) {
        *self.inner.lock() = Some(value);
    }
}

impl<S: subspace_networking::ProviderStorage + 'static> subspace_networking::ProviderStorage
    for MaybeProviderStorage<S>
{
    type ProvidedIter<'a> = std::iter::Empty<Cow<'a, ProviderRecord>>
    where S: 'a;

    fn provided(&self) -> Self::ProvidedIter<'_> {
        todo!()
    }

    fn remove_provider(
        &mut self,
        k: &subspace_networking::libp2p::kad::record::Key,
        p: &subspace_networking::libp2p::PeerId,
    ) {
        if let Some(x) = &mut *self.inner.lock() {
            x.remove_provider(k, p);
        }
    }

    fn providers(
        &self,
        key: &subspace_networking::libp2p::kad::record::Key,
    ) -> Vec<ProviderRecord> {
        self.inner.lock().as_ref().map(|x| x.providers(key)).unwrap_or_default()
    }

    fn add_provider(
        &mut self,
        record: ProviderRecord,
    ) -> subspace_networking::libp2p::kad::store::Result<()> {
        self.inner.lock().as_mut().map(|x| x.add_provider(record)).unwrap_or(Ok(()))
    }
}

pub struct AndProviderStorage<A, B> {
    a: A,
    b: B,
}

impl<A, B> AndProviderStorage<A, B> {
    pub fn new(a: A, b: B) -> Self {
        Self { a, b }
    }
}

impl<A: subspace_networking::ProviderStorage, B: subspace_networking::ProviderStorage>
    subspace_networking::ProviderStorage for AndProviderStorage<A, B>
{
    type ProvidedIter<'a> = std::iter::Chain<A::ProvidedIter<'a>, B::ProvidedIter<'a>>
    where A: 'a, B: 'a;

    fn add_provider(
        &mut self,
        record: ProviderRecord,
    ) -> subspace_networking::libp2p::kad::store::Result<()> {
        self.a.add_provider(record.clone())?;
        self.b.add_provider(record)?;
        Ok(())
    }

    fn provided(&self) -> Self::ProvidedIter<'_> {
        self.a.provided().chain(self.b.provided())
    }

    fn providers(
        &self,
        key: &subspace_networking::libp2p::kad::record::Key,
    ) -> Vec<ProviderRecord> {
        self.a
            .providers(key)
            .into_iter()
            .chain(self.b.providers(key))
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect()
    }

    fn remove_provider(
        &mut self,
        k: &subspace_networking::libp2p::kad::record::Key,
        p: &subspace_networking::libp2p::PeerId,
    ) {
        self.a.remove_provider(k, p);
        self.b.remove_provider(k, p);
    }
}

pub(crate) type FarmerProviderStorage =
    farmer_provider_storage::FarmerProviderStorage<ParityDbProviderStorage>;
pub(crate) type NodeProviderStorage<C> = node_provider_storage::NodeProviderStorage<
    PieceCache<C>,
    Either<ParityDbProviderStorage, subspace_networking::MemoryProviderStorage>,
>;
pub(crate) type ProviderStorage<C> =
    AndProviderStorage<MaybeProviderStorage<FarmerProviderStorage>, NodeProviderStorage<C>>;

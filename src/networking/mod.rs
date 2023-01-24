pub(crate) mod farmer_piece_receiver;
pub(crate) mod farmer_piece_storage;
pub(crate) mod farmer_provider_record_processor;
pub(crate) mod farmer_provider_storage;
pub(crate) mod node_provider_storage;
pub(crate) mod provider_storage_utils;

use either::*;
use provider_storage_utils::{AndProviderStorage, MaybeProviderStorage};
use subspace_core_primitives::{Piece, PieceIndexHash, SectorIndex};
use subspace_farmer::single_disk_plot::piece_reader::PieceReader;
use subspace_networking::libp2p::kad::record::Key;
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

pub(crate) type FarmerProviderStorage =
    farmer_provider_storage::FarmerProviderStorage<ParityDbProviderStorage>;
pub(crate) type NodeProviderStorage<C> = node_provider_storage::NodeProviderStorage<
    PieceCache<C>,
    Either<ParityDbProviderStorage, subspace_networking::MemoryProviderStorage>,
>;
pub(crate) type ProviderStorage<C> =
    AndProviderStorage<MaybeProviderStorage<FarmerProviderStorage>, NodeProviderStorage<C>>;

use std::sync::Arc;

use sc_client_api::AuxStore;
use subspace_core_primitives::{FlatPieces, Piece, PieceIndex, PieceIndexHash};
use subspace_networking::libp2p::kad::record::Key;
use subspace_networking::libp2p::kad::ProviderRecord;
use subspace_networking::libp2p::PeerId;
use subspace_networking::ProviderStorage;

pub(crate) struct NodePieceCache<C> {
    cache_size: u64,
    inner: subspace_service::piece_cache::PieceCache<C>,
}

impl<C> Clone for NodePieceCache<C> {
    fn clone(&self) -> Self {
        Self { cache_size: self.cache_size, inner: self.inner.clone() }
    }
}

impl<C: AuxStore> NodePieceCache<C> {
    pub fn new(
        aux_store: Arc<C>,
        cache_size: u64,
        local_peer_id: subspace_networking::libp2p::PeerId,
    ) -> Self {
        let inner =
            subspace_service::piece_cache::PieceCache::new(aux_store, cache_size, local_peer_id);
        Self { cache_size, inner }
    }

    pub fn get_piece(
        &self,
        piece_index_hash: PieceIndexHash,
    ) -> Result<Option<Piece>, Box<dyn std::error::Error>> {
        self.inner.get_piece(piece_index_hash)
    }

    pub fn add_pieces(
        &mut self,
        first_piece_index: PieceIndex,
        pieces: &FlatPieces,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.inner.add_pieces(first_piece_index, pieces)
    }
}

impl<C> subspace_farmer::utils::piece_cache::PieceCache for NodePieceCache<C>
where
    C: AuxStore + Send + Sync + 'static,
{
    type KeysIterator = std::iter::Empty<Key>;

    fn should_cache(&self, _: &Key) -> bool {
        false
    }

    fn add_piece(&mut self, _: Key, _: Piece) {}

    fn get_piece(&self, key: &Key) -> Option<Piece> {
        let piece_index_hash = <&[u8; 32]>::try_from(key.as_ref()).expect("Always 32 bytes long");
        self.inner.get_piece((*piece_index_hash).into()).ok().flatten()
    }

    fn keys(&self) -> Self::KeysIterator {
        Default::default()
    }
}

impl<C> ProviderStorage for NodePieceCache<C>
where
    C: AuxStore,
{
    type ProvidedIter<'a> =
        <subspace_service::piece_cache::PieceCache<C> as ProviderStorage>::ProvidedIter<'a>
        where subspace_service::piece_cache::PieceCache<C>: 'a;

    fn add_provider(
        &mut self,
        rec: ProviderRecord,
    ) -> subspace_networking::libp2p::kad::store::Result<()> {
        self.inner.add_provider(rec)
    }

    fn providers(&self, key: &Key) -> Vec<ProviderRecord> {
        self.inner.providers(key)
    }

    fn provided(&self) -> Self::ProvidedIter<'_> {
        self.inner.provided()
    }

    fn remove_provider(&mut self, key: &Key, peer_id: &PeerId) {
        self.inner.remove_provider(key, peer_id)
    }
}

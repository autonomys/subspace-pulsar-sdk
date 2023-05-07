/// Trait which abstracts farmer for node
#[async_trait::async_trait]
pub trait Farmer {
    /// Proof of space table
    type Table: subspace_proof_of_space::Table;

    /// Fetch piece by its hash
    async fn get_piece_by_hash(
        piece_index_hash: subspace_core_primitives::PieceIndexHash,
        piece_store: &sdk_dsn::builder::PieceStore,
        weak_readers_and_pieces: &std::sync::Weak<
            parking_lot::Mutex<
                Option<subspace_farmer::utils::readers_and_pieces::ReadersAndPieces>,
            >,
        >,
        piece_memory_cache: &subspace_farmer_components::piece_caching::PieceMemoryCache,
    ) -> Option<subspace_core_primitives::Piece>;
}

pub trait Node {
    type Client: sc_client_api::AuxStore + Send + Sync + 'static;
    type Table: subspace_proof_of_space::Table;
    type Rpc: subspace_farmer::node_client::NodeClient + Clone;

    fn name(&self) -> &str;
    fn dsn(&self) -> &sdk_dsn::builder::DsnShared<Self::Client>;
    fn rpc(&self) -> &Self::Rpc;
}

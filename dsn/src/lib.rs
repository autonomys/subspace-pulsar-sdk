//! Crate with DSN shared between sdk farmer and sdk node

#![warn(
    missing_docs,
    clippy::dbg_macro,
    clippy::unwrap_used,
    clippy::disallowed_types,
    unused_features
)]
#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![feature(concat_idents, const_option)]

mod builder;
mod provider_storage_utils;

pub use builder::*;
use either::*;
use sc_client_api::AuxStore;
/// Farmer piece cache
pub use subspace_farmer::utils::farmer_piece_cache::FarmerPieceCache;
use subspace_farmer_components::plotting::{PieceGetter, PieceGetterRetryPolicy};
use subspace_networking::utils::piece_provider::PieceValidator;
use subspace_networking::ParityDbProviderStorage;
use tracing::warn;

/// Node piece cache
pub type NodePieceCache<C> = subspace_service::piece_cache::PieceCache<C>;
/// Farmer provider storage
pub type FarmerProviderStorage =
    subspace_farmer::utils::farmer_provider_storage::FarmerProviderStorage<
        ParityDbProviderStorage,
        FarmerPieceCache,
    >;
/// Node provider storage
pub type NodeProviderStorage<C> = subspace_service::dsn::node_provider_storage::NodeProviderStorage<
    NodePieceCache<C>,
    Either<ParityDbProviderStorage, subspace_networking::MemoryProviderStorage>,
>;
/// General provider storage
pub type ProviderStorage<C> = provider_storage_utils::AndProviderStorage<
    provider_storage_utils::MaybeProviderStorage<FarmerProviderStorage>,
    NodeProviderStorage<C>,
>;

/// Node piece getter (combines DSN and Farmer getters)
pub struct NodePieceGetter<PV, C> {
    piece_getter: subspace_farmer::utils::node_piece_getter::NodePieceGetter<PV>,
    node_cache: NodePieceCache<C>,
}

impl<PV, C> NodePieceGetter<PV, C> {
    /// Constructor
    pub fn new(
        dsn_piece_getter: subspace_farmer::utils::node_piece_getter::NodePieceGetter<PV>,
        node_cache: NodePieceCache<C>,
    ) -> Self {
        Self { piece_getter: dsn_piece_getter, node_cache }
    }
}

#[async_trait::async_trait()]
impl<PV: PieceValidator, C: AuxStore + Send + Sync> PieceGetter for NodePieceGetter<PV, C> {
    async fn get_piece(
        &self,
        piece_index: subspace_core_primitives::PieceIndex,
        retry_policy: PieceGetterRetryPolicy,
    ) -> Result<
        Option<subspace_core_primitives::Piece>,
        Box<dyn std::error::Error + Send + Sync + 'static>,
    > {
        let piece = self.node_cache.get_piece(piece_index.hash()).map_err(|x| x.to_string())?;
        if piece.is_some() {
            return Ok(piece);
        }

        self.piece_getter.get_piece(piece_index, retry_policy).await
    }
}

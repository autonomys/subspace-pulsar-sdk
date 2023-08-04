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
/// Farmer piece cache
pub use subspace_farmer::utils::farmer_piece_cache::FarmerPieceCache;
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

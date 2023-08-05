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
/// Farmer piece cache
pub use subspace_farmer::utils::farmer_piece_cache::FarmerPieceCache;
use tracing::warn;

/// Node piece cache
pub type NodePieceCache<C> = subspace_service::piece_cache::PieceCache<C>;
/// Farmer provider storage
pub type FarmerProviderStorage =
    subspace_farmer::utils::farmer_provider_storage::FarmerProviderStorage<FarmerPieceCache>;

/// General provider storage
pub type ProviderStorage<C> = provider_storage_utils::AndProviderStorage<
    provider_storage_utils::MaybeProviderStorage<FarmerProviderStorage>,
    NodePieceCache<C>,
>;

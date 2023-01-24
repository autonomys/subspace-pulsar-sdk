pub(crate) mod farmer_provider_record_processor;
pub(crate) mod node_provider_storage;
pub(crate) mod provider_storage_utils;

use either::*;
use provider_storage_utils::{AndProviderStorage, MaybeProviderStorage};
use subspace_networking::ParityDbProviderStorage;

pub(crate) type FarmerProviderStorage =
    subspace_farmer::utils::farmer_provider_storage::FarmerProviderStorage<ParityDbProviderStorage>;
pub(crate) type NodeProviderStorage<C> = node_provider_storage::NodeProviderStorage<
    subspace_service::piece_cache::PieceCache<C>,
    Either<ParityDbProviderStorage, subspace_networking::MemoryProviderStorage>,
>;
pub(crate) type ProviderStorage<C> =
    AndProviderStorage<MaybeProviderStorage<FarmerProviderStorage>, NodeProviderStorage<C>>;

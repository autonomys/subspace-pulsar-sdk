use anyhow::Context;
use sc_rpc_api::state::StateApiClient;

use crate::utils::Rpc;

pub struct StorageKey(pub Vec<u8>);

impl StorageKey {
    pub fn new<IT, K>(keys: IT) -> Self
    where
        IT: IntoIterator<Item = K>,
        K: AsRef<[u8]>,
    {
        Self(keys.into_iter().flat_map(|key| sp_core_hashing::twox_128(key.as_ref())).collect())
    }

    pub fn events() -> Self {
        Self::new(["System", "Events"])
    }
}

impl Rpc {
    pub(crate) async fn get_storage<H>(
        &self,
        StorageKey(key): StorageKey,
        block: Option<H>,
    ) -> anyhow::Result<Option<sp_storage::StorageData>>
    where
        H: Send + Sync + 'static + serde::ser::Serialize + serde::de::DeserializeOwned,
    {
        self.storage(sp_storage::StorageKey(key), block)
            .await
            .context("Failed to fetch storage entry")
    }
}

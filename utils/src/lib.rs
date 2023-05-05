use std::pin::Pin;
use std::sync::Arc;

use anyhow::Context;
use derive_more::{Deref, DerefMut, Display, From, FromStr, Into};
use futures::prelude::*;
use jsonrpsee_core::client::{
    BatchResponse, ClientT, Subscription, SubscriptionClientT, SubscriptionKind,
};
use jsonrpsee_core::params::BatchRequestBuilder;
use jsonrpsee_core::server::rpc_module::RpcModule;
use jsonrpsee_core::traits::ToRpcParams;
use jsonrpsee_core::Error;
use sc_rpc_api::state::StateApiClient;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

mod rpc_client;

#[derive(Clone, Debug)]
pub struct Rpc {
    inner: Arc<RpcModule<()>>,
}

impl Rpc {
    pub fn new(handlers: &sc_service::RpcHandlers) -> Self {
        let inner = handlers.handle();
        Self { inner }
    }

    pub async fn subscribe_new_heads<'a, 'b, T>(
        &'a self,
    ) -> Result<impl Stream<Item = T::Header> + Send + Sync + Unpin + 'static, Error>
    where
        T: frame_system::Config + sp_runtime::traits::GetRuntimeBlockType,
        T::RuntimeBlock: serde::de::DeserializeOwned + sp_runtime::DeserializeOwned + 'static,
        T::Header: serde::de::DeserializeOwned + sp_runtime::DeserializeOwned + 'static,
        'a: 'b,
    {
        let stream = sc_rpc::chain::ChainApiClient::<
            T::BlockNumber,
            T::Hash,
            T::Header,
            sp_runtime::generic::SignedBlock<T::RuntimeBlock>,
        >::subscribe_new_heads(self)
        .await?
        .filter_map(|result| futures::future::ready(result.ok()));

        Ok(stream)
    }

    pub async fn subscribe_finalized_heads<'a, 'b, T>(
        &'a self,
    ) -> Result<impl Stream<Item = T::Header> + Send + Sync + Unpin + 'static, Error>
    where
        T: frame_system::Config + sp_runtime::traits::GetRuntimeBlockType,
        T::RuntimeBlock: serde::de::DeserializeOwned + sp_runtime::DeserializeOwned + 'static,
        T::Header: serde::de::DeserializeOwned + sp_runtime::DeserializeOwned + 'static,
        'a: 'b,
    {
        let stream = sc_rpc::chain::ChainApiClient::<
            T::BlockNumber,
            T::Hash,
            T::Header,
            sp_runtime::generic::SignedBlock<T::RuntimeBlock>,
        >::subscribe_finalized_heads(self)
        .await?
        .filter_map(|result| futures::future::ready(result.ok()));

        Ok(stream)
    }

    pub async fn get_events<T>(
        &self,
        block: Option<T::Hash>,
    ) -> anyhow::Result<Vec<frame_system::EventRecord<T::RuntimeEvent, T::Hash>>>
    where
        T: frame_system::Config,
        T::Hash: serde::ser::Serialize + serde::de::DeserializeOwned + Send + Sync + 'static,
        Vec<frame_system::EventRecord<T::RuntimeEvent, T::Hash>>: parity_scale_codec::Decode,
    {
        match self
            .get_storage::<T::Hash>(StorageKey::events(), block)
            .await
            .context("Failed to get events from storage")?
        {
            Some(sp_storage::StorageData(events)) =>
                parity_scale_codec::DecodeAll::decode_all(&mut events.as_ref())
                    .context("Failed to decode events"),
            None => Ok(vec![]),
        }
    }
}

#[async_trait::async_trait]
impl ClientT for Rpc {
    async fn notification<Params>(&self, method: &str, params: Params) -> Result<(), Error>
    where
        Params: ToRpcParams + Send,
    {
        self.inner.call(method, params).await
    }

    async fn request<R, Params>(&self, method: &str, params: Params) -> Result<R, Error>
    where
        R: DeserializeOwned,
        Params: ToRpcParams + Send,
    {
        self.inner.call(method, params).await
    }

    async fn batch_request<'a, R>(
        &self,
        _batch: BatchRequestBuilder<'a>,
    ) -> Result<BatchResponse<'a, R>, Error>
    where
        R: DeserializeOwned + std::fmt::Debug + 'a,
    {
        unreachable!("It isn't called at all")
    }
}

#[async_trait::async_trait]
impl SubscriptionClientT for Rpc {
    async fn subscribe<'a, Notif, Params>(
        &self,
        subscribe_method: &'a str,
        params: Params,
        _unsubscribe_method: &'a str,
    ) -> Result<jsonrpsee_core::client::Subscription<Notif>, Error>
    where
        Params: ToRpcParams + Send,
        Notif: DeserializeOwned,
    {
        let mut subscription = Arc::clone(&self.inner).subscribe(subscribe_method, params).await?;
        let kind = subscription.subscription_id().clone().into_owned();
        let (to_back, _) = futures::channel::mpsc::channel(10);
        let (mut notifs_tx, notifs_rx) = futures::channel::mpsc::channel(10);
        tokio::spawn(async move {
            while let Some(result) = subscription.next().await {
                let Ok((item, _)) = result else { break };
                if notifs_tx.send(item).await.is_err() {
                    break;
                }
            }
        });

        Ok(Subscription::new(to_back, notifs_rx, SubscriptionKind::Subscription(kind)))
    }

    async fn subscribe_to_method<'a, Notif>(
        &self,
        _method: &'a str,
    ) -> Result<jsonrpsee_core::client::Subscription<Notif>, Error>
    where
        Notif: DeserializeOwned,
    {
        unreachable!("It isn't called")
    }
}

pub fn is_default<T: Default + PartialEq>(t: &T) -> bool {
    t == &T::default()
}

struct Defer<F: FnOnce()>(Option<F>);

impl<F: FnOnce()> Defer<F> {
    pub fn new(f: F) -> Self {
        Self(Some(f))
    }
}

impl<F: FnOnce()> Drop for Defer<F> {
    fn drop(&mut self) {
        (self.0.take().expect("Always set"))();
    }
}

#[derive(Default, derivative::Derivative)]
#[derivative(Debug)]
pub struct DropCollection {
    #[derivative(Debug = "ignore")]
    vec: Vec<Box<dyn Send + Sync>>,
}

impl DropCollection {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn defer<F: FnOnce() + Sync + Send + 'static>(&mut self, f: F) {
        self.push(Defer::new(f))
    }

    pub fn push<T: Send + Sync + 'static>(&mut self, t: T) {
        self.vec.push(Box::new(t))
    }
}

impl<T: Send + Sync + 'static> FromIterator<T> for DropCollection {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        let mut me = Self::new();
        for item in iter {
            me.push(item);
        }
        me
    }
}

impl<T: Send + Sync + 'static> Extend<T> for DropCollection {
    fn extend<I: IntoIterator<Item = T>>(&mut self, iter: I) {
        for item in iter {
            self.push(item);
        }
    }
}

#[derive(Default, derivative::Derivative)]
#[derivative(Debug)]
pub struct AsyncDropFutures {
    #[derivative(Debug = "ignore")]
    vec: Vec<Pin<Box<dyn Future<Output = ()> + Send + Sync>>>,
}

impl AsyncDropFutures {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push<F: Future<Output = ()> + Send + Sync + 'static>(&mut self, fut: F) {
        self.vec.push(Box::pin(fut))
    }

    pub async fn async_drop(self) {
        for f in self.vec {
            f.await;
        }
    }
}

/// Container for number of bytes.
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    Deref,
    DerefMut,
    Deserialize,
    Display,
    Eq,
    From,
    FromStr,
    Into,
    Ord,
    PartialEq,
    PartialOrd,
    Serialize,
)]
#[serde(transparent)]
pub struct ByteSize(#[serde(with = "bytesize_serde")] pub bytesize::ByteSize);

impl ByteSize {
    /// Constructor for bytes
    pub const fn b(n: u64) -> Self {
        Self(bytesize::ByteSize::b(n))
    }

    /// Constructor for kilobytes
    pub const fn kb(n: u64) -> Self {
        Self(bytesize::ByteSize::kb(n))
    }

    /// Constructor for kibibytes
    pub const fn kib(n: u64) -> Self {
        Self(bytesize::ByteSize::kib(n))
    }

    /// Constructor for megabytes
    pub const fn mb(n: u64) -> Self {
        Self(bytesize::ByteSize::mb(n))
    }

    /// Constructor for mibibytes
    pub const fn mib(n: u64) -> Self {
        Self(bytesize::ByteSize::mib(n))
    }

    /// Constructor for gigabytes
    pub const fn gb(n: u64) -> Self {
        Self(bytesize::ByteSize::gb(n))
    }

    /// Constructor for gibibytes
    pub const fn gib(n: u64) -> Self {
        Self(bytesize::ByteSize::gib(n))
    }
}

/// Multiaddr is a wrapper around libp2p one
#[derive(
    Clone,
    Debug,
    Deref,
    DerefMut,
    Deserialize,
    Display,
    Eq,
    From,
    FromStr,
    Into,
    PartialEq,
    Serialize,
)]
#[serde(transparent)]
pub struct Multiaddr(pub libp2p_core::Multiaddr);

impl From<sc_network::Multiaddr> for Multiaddr {
    fn from(multiaddr: sc_network::Multiaddr) -> Self {
        multiaddr.to_string().parse().expect("Conversion between 2 libp2p versions is always right")
    }
}

impl From<Multiaddr> for sc_network::Multiaddr {
    fn from(multiaddr: Multiaddr) -> Self {
        multiaddr.to_string().parse().expect("Conversion between 2 libp2p versions is always right")
    }
}

/// Multiaddr with peer id
#[derive(
    Debug, Clone, Deserialize, Serialize, PartialEq, From, Into, FromStr, Deref, DerefMut, Display,
)]
#[serde(transparent)]
pub struct MultiaddrWithPeerId(pub sc_service::config::MultiaddrWithPeerId);

impl MultiaddrWithPeerId {
    /// Constructor for peer id
    pub fn new(multiaddr: impl Into<Multiaddr>, peer_id: sc_network::PeerId) -> Self {
        Self(sc_service::config::MultiaddrWithPeerId {
            multiaddr: multiaddr.into().into(),
            peer_id,
        })
    }
}

impl From<MultiaddrWithPeerId> for sc_network::Multiaddr {
    fn from(multiaddr: MultiaddrWithPeerId) -> Self {
        multiaddr.to_string().parse().expect("Conversion between 2 libp2p versions is always right")
    }
}

impl From<MultiaddrWithPeerId> for libp2p_core::Multiaddr {
    fn from(multiaddr: MultiaddrWithPeerId) -> Self {
        multiaddr.to_string().parse().expect("Conversion between 2 libp2p versions is always right")
    }
}

impl From<MultiaddrWithPeerId> for Multiaddr {
    fn from(multiaddr: MultiaddrWithPeerId) -> Self {
        multiaddr.to_string().parse().expect("Conversion between 2 libp2p versions is always right")
    }
}

#[cfg(not(tokio_unstable))]
pub fn task_spawn<F>(name: impl AsRef<str>, future: F) -> tokio::task::JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let _ = name;
    tokio::task::spawn(future)
}

#[cfg(tokio_unstable)]
pub fn task_spawn<F>(name: impl AsRef<str>, future: F) -> tokio::task::JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    tokio::task::Builder::new()
        .name(name.as_ref())
        .spawn(future)
        .expect("Spawning task never fails")
}

#[cfg(not(tokio_unstable))]
pub fn task_spawn_blocking<F, R>(name: impl AsRef<str>, f: F) -> tokio::task::JoinHandle<R>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    let _ = name;
    tokio::task::spawn_blocking(f)
}

#[cfg(tokio_unstable)]
pub fn task_spawn_blocking<F, R>(name: impl AsRef<str>, f: F) -> tokio::task::JoinHandle<R>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    tokio::task::Builder::new()
        .name(name.as_ref())
        .spawn_blocking(f)
        .expect("Spawning task never fails")
}

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

pub mod chain_spec {
    use frame_support::traits::Get;
    use sc_service::Properties;
    use sp_core::crypto::AccountId32;
    use sp_core::{sr25519, Pair, Public};
    use sp_runtime::traits::IdentifyAccount;
    use sp_runtime::MultiSigner;
    use subspace_runtime::SS58Prefix;
    use subspace_runtime_primitives::DECIMAL_PLACES;

    /// Shared chain spec properties related to the coin.
    pub fn chain_spec_properties() -> Properties {
        let mut properties = Properties::new();

        properties.insert("ss58Format".into(), <SS58Prefix as Get<u16>>::get().into());
        properties.insert("tokenDecimals".into(), DECIMAL_PLACES.into());
        properties.insert("tokenSymbol".into(), "tSSC".into());

        properties
    }

    /// Get public key from keypair seed.
    pub fn get_public_key_from_seed<TPublic: Public>(
        seed: &'static str,
    ) -> <TPublic::Pair as Pair>::Public {
        TPublic::Pair::from_string(&format!("//{seed}"), None)
            .expect("Static values are valid; qed")
            .public()
    }

    /// Generate an account ID from seed.
    pub fn get_account_id_from_seed(seed: &'static str) -> AccountId32 {
        MultiSigner::from(get_public_key_from_seed::<sr25519::Public>(seed)).into_account()
    }
}

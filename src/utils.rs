use std::sync::Arc;

use futures::prelude::*;
use jsonrpsee_core::client::{
    BatchResponse, ClientT, Subscription, SubscriptionClientT, SubscriptionKind,
};
use jsonrpsee_core::params::BatchRequestBuilder;
use jsonrpsee_core::server::rpc_module::RpcModule;
use jsonrpsee_core::traits::ToRpcParams;
use jsonrpsee_core::Error;
use serde::de::DeserializeOwned;
use sp_runtime::traits::{Block as BlockT, Header as HeaderT};
use subspace_runtime_primitives::opaque::Block;

#[cfg(test)]
pub(crate) fn test_init() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            "info,subspace_sdk=trace,subspace_farmer=trace,subspace_service=trace"
                .parse::<tracing_subscriber::EnvFilter>()
                .unwrap(),
        )
        .with_test_writer()
        .try_init();
}

#[derive(Clone, Debug)]
pub(crate) struct Rpc {
    inner: Arc<RpcModule<()>>,
}

impl Rpc {
    pub fn new(handlers: &sc_service::RpcHandlers) -> Self {
        let inner = handlers.handle();
        Self { inner }
    }

    pub(crate) async fn subscribe_new_blocks<'a: 'b, 'b>(
        &'a self,
    ) -> Result<
        impl Stream<Item = crate::node::BlockNotification> + Send + Sync + Unpin + 'static,
        Error,
    > {
        let stream = sc_rpc::chain::ChainApiClient::<
            <<Block as BlockT>::Header as HeaderT>::Number,
            <Block as BlockT>::Hash,
            <Block as BlockT>::Header,
            sp_runtime::generic::SignedBlock<Block>,
        >::subscribe_new_heads(self)
        .await?
        .filter_map(|result| futures::future::ready(result.ok()))
        .map(Into::into);

        Ok(stream)
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
    pub(crate) fn chain_spec_properties() -> Properties {
        let mut properties = Properties::new();

        properties.insert("ss58Format".into(), <SS58Prefix as Get<u16>>::get().into());
        properties.insert("tokenDecimals".into(), DECIMAL_PLACES.into());
        properties.insert("tokenSymbol".into(), "tSSC".into());

        properties
    }

    /// Get public key from keypair seed.
    pub(crate) fn get_public_key_from_seed<TPublic: Public>(
        seed: &'static str,
    ) -> <TPublic::Pair as Pair>::Public {
        TPublic::Pair::from_string(&format!("//{seed}"), None)
            .expect("Static values are valid; qed")
            .public()
    }

    /// Generate an account ID from seed.
    pub(crate) fn get_account_id_from_seed(seed: &'static str) -> AccountId32 {
        MultiSigner::from(get_public_key_from_seed::<sr25519::Public>(seed)).into_account()
    }
}

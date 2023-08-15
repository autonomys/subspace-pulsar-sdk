use std::sync::Arc;

use derivative::Derivative;
use parking_lot::RwLock;
use subspace_networking::libp2p::kad::record::Key;
use subspace_networking::libp2p::kad::{ProviderRecord, RecordKey};
use subspace_networking::LocalRecordProvider;

#[derive(Derivative)]
#[derivative(Debug)]
pub struct MaybeLocalRecordProvider<S> {
    #[derivative(Debug = "ignore")]
    inner: Arc<RwLock<Option<S>>>,
}

impl<S> Clone for MaybeLocalRecordProvider<S> {
    fn clone(&self) -> Self {
        Self { inner: Arc::clone(&self.inner) }
    }
}

impl<S> MaybeLocalRecordProvider<S> {
    pub fn new(inner: Arc<RwLock<Option<S>>>) -> Self {
        Self { inner }
    }
}

impl<S: LocalRecordProvider + 'static> LocalRecordProvider for MaybeLocalRecordProvider<S> {
    fn record(&self, key: &RecordKey) -> Option<ProviderRecord> {
        self.inner.read().as_ref().map(|v| v.record(key)).unwrap_or(None)
    }
}

#[derive(Clone, Debug)]
pub struct AndLocalRecordProvider<A, B> {
    a: A,
    b: B,
}

impl<A, B> AndLocalRecordProvider<A, B> {
    pub fn new(a: A, b: B) -> Self {
        Self { a, b }
    }
}

impl<A: LocalRecordProvider, B: LocalRecordProvider> LocalRecordProvider
    for AndLocalRecordProvider<A, B>
{
    fn record(&self, key: &Key) -> Option<ProviderRecord> {
        self.a.record(key).or(self.b.record(key))
    }
}

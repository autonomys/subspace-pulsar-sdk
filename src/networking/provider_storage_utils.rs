use std::borrow::Cow;
use std::sync::Arc;

use derivative::Derivative;
use parking_lot::Mutex;
use subspace_networking::libp2p::kad::ProviderRecord;

#[derive(Derivative)]
#[derivative(Debug)]
pub struct MaybeProviderStorage<S> {
    #[derivative(Debug = "ignore")]
    inner: Arc<Mutex<Option<S>>>,
}

impl<S> Clone for MaybeProviderStorage<S> {
    fn clone(&self) -> Self {
        Self { inner: Arc::clone(&self.inner) }
    }
}

impl<S> MaybeProviderStorage<S> {
    pub fn none() -> Self {
        Self { inner: Arc::new(Mutex::new(None)) }
    }

    pub fn swap(&self, value: S) {
        *self.inner.lock() = Some(value);
    }
}

impl<S: subspace_networking::ProviderStorage + 'static> subspace_networking::ProviderStorage
    for MaybeProviderStorage<S>
{
    type ProvidedIter<'a> = std::iter::Empty<Cow<'a, ProviderRecord>>
    where S: 'a;

    fn provided(&self) -> Self::ProvidedIter<'_> {
        todo!()
    }

    fn remove_provider(
        &mut self,
        k: &subspace_networking::libp2p::kad::record::Key,
        p: &subspace_networking::libp2p::PeerId,
    ) {
        if let Some(x) = &mut *self.inner.lock() {
            x.remove_provider(k, p);
        }
    }

    fn providers(
        &self,
        key: &subspace_networking::libp2p::kad::record::Key,
    ) -> Vec<ProviderRecord> {
        self.inner.lock().as_ref().map(|x| x.providers(key)).unwrap_or_default()
    }

    fn add_provider(
        &mut self,
        record: ProviderRecord,
    ) -> subspace_networking::libp2p::kad::store::Result<()> {
        self.inner.lock().as_mut().map(|x| x.add_provider(record)).unwrap_or(Ok(()))
    }
}

pub struct AndProviderStorage<A, B> {
    a: A,
    b: B,
}

impl<A, B> AndProviderStorage<A, B> {
    pub fn new(a: A, b: B) -> Self {
        Self { a, b }
    }
}

impl<A: subspace_networking::ProviderStorage, B: subspace_networking::ProviderStorage>
    subspace_networking::ProviderStorage for AndProviderStorage<A, B>
{
    type ProvidedIter<'a> = std::iter::Chain<A::ProvidedIter<'a>, B::ProvidedIter<'a>>
    where A: 'a, B: 'a;

    fn add_provider(
        &mut self,
        record: ProviderRecord,
    ) -> subspace_networking::libp2p::kad::store::Result<()> {
        self.a.add_provider(record.clone())?;
        self.b.add_provider(record)?;
        Ok(())
    }

    fn provided(&self) -> Self::ProvidedIter<'_> {
        self.a.provided().chain(self.b.provided())
    }

    fn providers(
        &self,
        key: &subspace_networking::libp2p::kad::record::Key,
    ) -> Vec<ProviderRecord> {
        self.a
            .providers(key)
            .into_iter()
            .chain(self.b.providers(key))
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect()
    }

    fn remove_provider(
        &mut self,
        k: &subspace_networking::libp2p::kad::record::Key,
        p: &subspace_networking::libp2p::PeerId,
    ) {
        self.a.remove_provider(k, p);
        self.b.remove_provider(k, p);
    }
}

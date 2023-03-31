use std::num::NonZeroUsize;
use std::path::Path;

use derivative::Derivative;
use derive_builder::Builder;
use derive_more::{Deref, DerefMut, Display, From};
use serde::{Deserialize, Serialize};

use super::substrate::{
    Base, BaseBuilder, ExecutionStrategy, NetworkBuilder, OffchainWorkerBuilder, RpcBuilder,
    StorageMonitor,
};
use super::{domains, ChainSpec, Node};
use crate::dsn::builder::{Dsn, DsnBuilder};

/// Wrapper with default value for piece cache size
#[derive(
    Debug, Clone, Derivative, Deserialize, Serialize, PartialEq, Eq, From, Deref, DerefMut, Display,
)]
#[derivative(Default)]
#[serde(transparent)]
pub struct PieceCacheSize(
    #[derivative(Default(value = "bytesize::ByteSize::gib(1)"))]
    #[serde(with = "bytesize_serde")]
    pub(crate) bytesize::ByteSize,
);

/// Wrapper with default value for segment publish concurent jobs
#[derive(
    Debug, Clone, Derivative, Deserialize, Serialize, PartialEq, Eq, From, Deref, DerefMut, Display,
)]
#[derivative(Default)]
#[serde(transparent)]
pub struct SegmentPublishConcurrency(
    #[derivative(Default(value = "NonZeroUsize::new(10).expect(\"10 > 0\")"))]
    pub(crate)  NonZeroUsize,
);

/// Node builder
#[derive(Debug, Clone, Derivative, Builder, Deserialize, Serialize, PartialEq)]
#[derivative(Default)]
#[builder(pattern = "immutable", build_fn(private, name = "_build"), name = "Builder")]
#[non_exhaustive]
pub struct Config {
    /// Set piece cache size
    #[builder(setter(into), default)]
    #[serde(default, skip_serializing_if = "crate::utils::is_default")]
    pub piece_cache_size: PieceCacheSize,
    /// Max number of segments that can be published concurrently, impacts
    /// RAM usage and network bandwidth.
    #[builder(setter(into), default)]
    #[serde(default, skip_serializing_if = "crate::utils::is_default")]
    pub segment_publish_concurrency: SegmentPublishConcurrency,
    /// Should we sync blocks from the DSN?
    #[builder(default)]
    #[serde(default, skip_serializing_if = "crate::utils::is_default")]
    pub sync_from_dsn: bool,
    #[doc(hidden)]
    #[builder(
        setter(into, strip_option),
        field(type = "BaseBuilder", build = "self.base.build()")
    )]
    #[serde(flatten, skip_serializing_if = "crate::utils::is_default")]
    pub base: Base,
    /// System domain settings
    #[builder(setter(into, strip_option), default)]
    #[serde(default, skip_serializing_if = "crate::utils::is_default")]
    pub system_domain: Option<domains::Config>,
    /// DSN settings
    #[builder(setter(into), default)]
    #[serde(default, skip_serializing_if = "crate::utils::is_default")]
    pub dsn: Dsn,
    /// Storage monitor settings
    #[builder(setter(into), default)]
    #[serde(default, skip_serializing_if = "crate::utils::is_default")]
    pub storage_monitor: Option<StorageMonitor>,
}

impl Config {
    /// Dev configuraiton
    pub fn dev() -> Builder {
        Builder::dev()
    }

    /// Gemini 3d configuraiton
    pub fn gemini_3d() -> Builder {
        Builder::gemini_3d()
    }

    /// Devnet configuraiton
    pub fn devnet() -> Builder {
        Builder::devnet()
    }
}

impl Builder {
    /// Dev chain configuration
    pub fn dev() -> Self {
        Self::new()
            .force_authoring(true)
            .network(NetworkBuilder::dev())
            .dsn(DsnBuilder::dev())
            .rpc(RpcBuilder::dev())
            .offchain_worker(OffchainWorkerBuilder::dev())
    }

    /// Gemini 3d configuration
    pub fn gemini_3d() -> Self {
        Self::new()
            .execution_strategy(ExecutionStrategy::AlwaysWasm)
            .network(NetworkBuilder::gemini_3d())
            .dsn(DsnBuilder::gemini_3d())
            .rpc(RpcBuilder::gemini_3d())
            .offchain_worker(OffchainWorkerBuilder::gemini_3d())
    }

    /// Devnet chain configuration
    pub fn devnet() -> Self {
        Self::new()
            .execution_strategy(ExecutionStrategy::AlwaysWasm)
            .network(NetworkBuilder::devnet())
            .dsn(DsnBuilder::devnet())
            .rpc(RpcBuilder::devnet())
            .offchain_worker(OffchainWorkerBuilder::devnet())
    }

    /// Get configuration for saving on disk
    pub fn configuration(&self) -> Config {
        self._build().expect("Build is infallible")
    }

    /// New builder
    pub fn new() -> Self {
        Self::default()
    }

    /// Start a node with supplied parameters
    pub async fn build(
        self,
        directory: impl AsRef<Path>,
        chain_spec: ChainSpec,
    ) -> anyhow::Result<Node> {
        self.configuration().build(directory, chain_spec).await
    }
}

crate::derive_base!(Base => Builder);

use derivative::Derivative;
use derive_builder::Builder;
use derive_more::{Deref, DerefMut, Display, From};
use libp2p_core::Multiaddr;
use sc_network_common::config::MultiaddrWithPeerId;
use serde::{Deserialize, Serialize};

/// Wrapper with default value for listen address
#[derive(
    Debug, Clone, Derivative, Deserialize, Serialize, PartialEq, Eq, From, Deref, DerefMut,
)]
#[derivative(Default)]
#[serde(transparent)]
pub struct ListenAddresses(
    #[derivative(Default(
        // TODO: get rid of it, once it won't be required by monorepo
        value = "vec![\"/ip4/127.0.0.1/tcp/0\".parse().expect(\"Always valid\")]"
    ))]
    pub(crate) Vec<Multiaddr>,
);

/// Wrapper with default value for number of incoming connections
#[derive(
    Debug, Clone, Derivative, Deserialize, Serialize, PartialEq, Eq, From, Deref, DerefMut, Display,
)]
#[derivative(Default)]
#[serde(transparent)]
pub struct InConnections(#[derivative(Default(value = "100"))] pub(crate) u32);

/// Wrapper with default value for number of outgoing connections
#[derive(
    Debug, Clone, Derivative, Deserialize, Serialize, PartialEq, Eq, From, Deref, DerefMut, Display,
)]
#[derivative(Default)]
#[serde(transparent)]
pub struct OutConnections(#[derivative(Default(value = "100"))] pub(crate) u32);

/// Wrapper with default value for number of target connections
#[derive(
    Debug, Clone, Derivative, Deserialize, Serialize, PartialEq, Eq, From, Deref, DerefMut, Display,
)]
#[derivative(Default)]
#[serde(transparent)]
pub struct TargetConnections(#[derivative(Default(value = "50"))] pub(crate) u32);

/// Wrapper with default value for number of pending incoming connections
#[derive(
    Debug, Clone, Derivative, Deserialize, Serialize, PartialEq, Eq, From, Deref, DerefMut, Display,
)]
#[derivative(Default)]
#[serde(transparent)]
pub struct PendingInConnections(#[derivative(Default(value = "100"))] pub(crate) u32);

/// Wrapper with default value for number of pending outgoing connections
#[derive(
    Debug, Clone, Derivative, Deserialize, Serialize, PartialEq, Eq, From, Deref, DerefMut, Display,
)]
#[derivative(Default)]
#[serde(transparent)]
pub struct PendingOutConnections(#[derivative(Default(value = "100"))] pub(crate) u32);

/// Node DSN builder
#[derive(Debug, Clone, Derivative, Builder, Deserialize, Serialize, PartialEq)]
#[derivative(Default)]
#[builder(pattern = "immutable", build_fn(private, name = "_build"), name = "DsnBuilder")]
#[non_exhaustive]
pub struct Dsn {
    /// Listen on some address for other nodes
    #[builder(default, setter(into, strip_option))]
    #[serde(default, skip_serializing_if = "crate::utils::is_default")]
    pub provider_storage_path: Option<std::path::PathBuf>,
    /// Listen on some address for other nodes
    #[builder(default, setter(into))]
    #[serde(default, skip_serializing_if = "crate::utils::is_default")]
    pub listen_addresses: ListenAddresses,
    /// Boot nodes
    #[builder(default)]
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub boot_nodes: Vec<MultiaddrWithPeerId>,
    /// Reserved nodes
    #[builder(default)]
    #[serde(default, skip_serializing_if = "crate::utils::is_default")]
    pub reserved_nodes: Vec<Multiaddr>,
    /// Determines whether we allow keeping non-global (private, shared,
    /// loopback..) addresses in Kademlia DHT.
    #[builder(default)]
    #[serde(default, skip_serializing_if = "crate::utils::is_default")]
    pub allow_non_global_addresses_in_dht: bool,
    /// Defines max established incoming swarm connection limit.
    #[builder(setter(into), default)]
    #[serde(default, skip_serializing_if = "crate::utils::is_default")]
    pub in_connections: InConnections,
    /// Defines max established outgoing swarm connection limit.
    #[builder(setter(into), default)]
    #[serde(default, skip_serializing_if = "crate::utils::is_default")]
    pub out_connections: OutConnections,
    /// Pending incoming swarm connection limit.
    #[builder(setter(into), default)]
    #[serde(default, skip_serializing_if = "crate::utils::is_default")]
    pub pending_in_connections: PendingInConnections,
    /// Pending outgoing swarm connection limit.
    #[builder(setter(into), default)]
    #[serde(default, skip_serializing_if = "crate::utils::is_default")]
    pub pending_out_connections: PendingOutConnections,
    /// Defines target total (in and out) connection number for DSN that
    /// should be maintained.
    #[builder(setter(into), default)]
    #[serde(default, skip_serializing_if = "crate::utils::is_default")]
    pub target_connections: TargetConnections,
}

crate::generate_builder!(Dsn);

impl DsnBuilder {
    /// Dev chain configuration
    pub fn dev() -> Self {
        Self::new().allow_non_global_addresses_in_dht(true)
    }

    /// Gemini 3d configuration
    pub fn gemini_3d() -> Self {
        Self::new().listen_addresses(vec![
            "/ip6/::/tcp/30433".parse().expect("hardcoded value is true"),
            "/ip4/0.0.0.0/tcp/30433".parse().expect("hardcoded value is true"),
        ])
    }

    /// Gemini 3d configuration
    pub fn devnet() -> Self {
        Self::new().listen_addresses(vec![
            "/ip6/::/tcp/30433".parse().expect("hardcoded value is true"),
            "/ip4/0.0.0.0/tcp/30433".parse().expect("hardcoded value is true"),
        ])
    }
}

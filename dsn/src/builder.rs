use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Weak};

use anyhow::Context;
use derivative::Derivative;
use derive_builder::Builder;
use derive_more::{Deref, DerefMut, Display, From};
use futures::prelude::*;
use sc_consensus_subspace::archiver::SegmentHeadersStore;
use sdk_utils::{self, DestructorSet, Multiaddr, MultiaddrWithPeerId};
use serde::{Deserialize, Serialize};
use subspace_core_primitives::Piece;
use subspace_farmer::piece_cache::PieceCache as FarmerPieceCache;
use subspace_farmer::utils::archival_storage_info::ArchivalStorageInfo;
use subspace_farmer::utils::archival_storage_pieces::ArchivalStoragePieces;
use subspace_farmer::utils::readers_and_pieces::ReadersAndPieces;
use subspace_networking::utils::strip_peer_id;
use subspace_networking::{
    PeerInfo, PeerInfoProvider, PieceByIndexRequest, PieceByIndexRequestHandler,
    PieceByIndexResponse, SegmentHeaderBySegmentIndexesRequestHandler, SegmentHeaderRequest,
    SegmentHeaderResponse,
};

use super::local_provider_record_utils::MaybeLocalRecordProvider;
use super::LocalRecordProvider;

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
    pub Vec<Multiaddr>,
);

/// Wrapper with default value for number of incoming connections
#[derive(
    Debug, Clone, Derivative, Deserialize, Serialize, PartialEq, Eq, From, Deref, DerefMut, Display,
)]
#[derivative(Default)]
#[serde(transparent)]
pub struct InConnections(#[derivative(Default(value = "100"))] pub u32);

/// Wrapper with default value for number of outgoing connections
#[derive(
    Debug, Clone, Derivative, Deserialize, Serialize, PartialEq, Eq, From, Deref, DerefMut, Display,
)]
#[derivative(Default)]
#[serde(transparent)]
pub struct OutConnections(#[derivative(Default(value = "100"))] pub u32);

/// Wrapper with default value for number of target connections
#[derive(
    Debug, Clone, Derivative, Deserialize, Serialize, PartialEq, Eq, From, Deref, DerefMut, Display,
)]
#[derivative(Default)]
#[serde(transparent)]
pub struct TargetConnections(#[derivative(Default(value = "50"))] pub u32);

/// Wrapper with default value for number of pending incoming connections
#[derive(
    Debug, Clone, Derivative, Deserialize, Serialize, PartialEq, Eq, From, Deref, DerefMut, Display,
)]
#[derivative(Default)]
#[serde(transparent)]
pub struct PendingInConnections(#[derivative(Default(value = "100"))] pub u32);

/// Wrapper with default value for number of pending outgoing connections
#[derive(
    Debug, Clone, Derivative, Deserialize, Serialize, PartialEq, Eq, From, Deref, DerefMut, Display,
)]
#[derivative(Default)]
#[serde(transparent)]
pub struct PendingOutConnections(#[derivative(Default(value = "100"))] pub u32);

/// Node DSN builder
#[derive(Debug, Clone, Derivative, Builder, Deserialize, Serialize, PartialEq)]
#[derivative(Default)]
#[builder(pattern = "immutable", build_fn(private, name = "_build"), name = "DsnBuilder")]
#[non_exhaustive]
pub struct Dsn {
    /// Listen on some address for other nodes
    #[builder(default, setter(into, strip_option))]
    #[serde(default, skip_serializing_if = "sdk_utils::is_default")]
    pub provider_storage_path: Option<std::path::PathBuf>,
    /// Listen on some address for other nodes
    #[builder(default, setter(into))]
    #[serde(default, skip_serializing_if = "sdk_utils::is_default")]
    pub listen_addresses: ListenAddresses,
    /// Boot nodes
    #[builder(default)]
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub boot_nodes: Vec<MultiaddrWithPeerId>,
    /// Known external addresses
    #[builder(setter(into), default)]
    #[serde(default, skip_serializing_if = "sdk_utils::is_default")]
    pub external_addresses: Vec<Multiaddr>,
    /// Reserved nodes
    #[builder(default)]
    #[serde(default, skip_serializing_if = "sdk_utils::is_default")]
    pub reserved_nodes: Vec<Multiaddr>,
    /// Determines whether we allow keeping non-global (private, shared,
    /// loopback..) addresses in Kademlia DHT.
    #[builder(default)]
    #[serde(default, skip_serializing_if = "sdk_utils::is_default")]
    pub allow_non_global_addresses_in_dht: bool,
    /// Defines max established incoming swarm connection limit.
    #[builder(setter(into), default)]
    #[serde(default, skip_serializing_if = "sdk_utils::is_default")]
    pub in_connections: InConnections,
    /// Defines max established outgoing swarm connection limit.
    #[builder(setter(into), default)]
    #[serde(default, skip_serializing_if = "sdk_utils::is_default")]
    pub out_connections: OutConnections,
    /// Pending incoming swarm connection limit.
    #[builder(setter(into), default)]
    #[serde(default, skip_serializing_if = "sdk_utils::is_default")]
    pub pending_in_connections: PendingInConnections,
    /// Pending outgoing swarm connection limit.
    #[builder(setter(into), default)]
    #[serde(default, skip_serializing_if = "sdk_utils::is_default")]
    pub pending_out_connections: PendingOutConnections,
    /// Defines target total (in and out) connection number for DSN that
    /// should be maintained.
    #[builder(setter(into), default)]
    #[serde(default, skip_serializing_if = "sdk_utils::is_default")]
    pub target_connections: TargetConnections,
}

sdk_utils::generate_builder!(Dsn);

impl DsnBuilder {
    /// Dev chain configuration
    pub fn dev() -> Self {
        Self::new().allow_non_global_addresses_in_dht(true)
    }

    /// Gemini 3e configuration
    pub fn gemini_3e() -> Self {
        Self::new().listen_addresses(vec![
            "/ip6/::/tcp/30433".parse().expect("hardcoded value is true"),
            "/ip4/0.0.0.0/tcp/30433".parse().expect("hardcoded value is true"),
        ])
    }

    /// Gemini 3e configuration
    pub fn devnet() -> Self {
        Self::new().listen_addresses(vec![
            "/ip6/::/tcp/30433".parse().expect("hardcoded value is true"),
            "/ip4/0.0.0.0/tcp/30433".parse().expect("hardcoded value is true"),
        ])
    }
}

/// Options for DSN
pub struct DsnOptions<C, PieceByHash, SegmentHeaderByIndexes> {
    /// Client to aux storage for node piece cache
    pub client: Arc<C>,
    /// Path for dsn
    pub base_path: PathBuf,
    /// Keypair for networking
    pub keypair: subspace_networking::libp2p::identity::Keypair,
    /// Get piece by hash handler
    pub get_piece_by_index: PieceByHash,
    /// Get segment header by segment indexes handler
    pub get_segment_header_by_segment_indexes: SegmentHeaderByIndexes,
    /// Farmer total allocated space across all plots
    pub farmer_total_space_pledged: usize,
    /// Segment header store
    pub segment_header_store: SegmentHeadersStore<C>,
}

/// Shared Dsn structure between node and farmer
#[derive(Derivative)]
#[derivative(Debug)]
pub struct DsnShared {
    /// Dsn node
    pub node: subspace_networking::Node,
    /// Farmer readers and pieces
    pub farmer_readers_and_pieces: Arc<parking_lot::Mutex<Option<ReadersAndPieces>>>,
    /// Farmer piece cache
    pub farmer_piece_cache: Arc<parking_lot::RwLock<Option<FarmerPieceCache>>>,
    /// Farmer archival storage pieces
    pub farmer_archival_storage_pieces: ArchivalStoragePieces,
    /// Farmer archival storage info
    pub farmer_archival_storage_info: ArchivalStorageInfo,
    _destructors: DestructorSet,
}

impl Dsn {
    /// Build dsn
    pub fn build_dsn<B, C, PieceByIndex, F1, SegmentHeaderByIndexes>(
        self,
        options: DsnOptions<C, PieceByIndex, SegmentHeaderByIndexes>,
    ) -> anyhow::Result<(DsnShared, subspace_networking::NodeRunner<LocalRecordProvider>)>
    where
        B: sp_runtime::traits::Block,
        C: sc_client_api::AuxStore + sp_blockchain::HeaderBackend<B> + Send + Sync + 'static,
        PieceByIndex: Fn(
                &PieceByIndexRequest,
                Weak<parking_lot::Mutex<Option<ReadersAndPieces>>>,
                Arc<parking_lot::RwLock<Option<FarmerPieceCache>>>,
            ) -> F1
            + Send
            + Sync
            + 'static,
        F1: Future<Output = Option<PieceByIndexResponse>> + Send + 'static,
        SegmentHeaderByIndexes: Fn(&SegmentHeaderRequest, &SegmentHeadersStore<C>) -> Option<SegmentHeaderResponse>
            + Send
            + Sync
            + 'static,
    {
        let DsnOptions {
            client,
            base_path,
            keypair,
            get_piece_by_index,
            get_segment_header_by_segment_indexes,
            farmer_total_space_pledged,
            segment_header_store,
        } = options;
        let farmer_readers_and_pieces = Arc::new(parking_lot::Mutex::new(None));
        let protocol_version = hex::encode(client.info().genesis_hash);
        let farmer_piece_cache = Arc::new(parking_lot::RwLock::new(None));
        let local_records_provider = MaybeLocalRecordProvider::new(farmer_piece_cache.clone());

        let cuckoo_filter_size = farmer_total_space_pledged / Piece::SIZE + 1usize;
        let farmer_archival_storage_pieces = ArchivalStoragePieces::new(cuckoo_filter_size);
        let farmer_archival_storage_info = ArchivalStorageInfo::default();

        tracing::debug!(genesis_hash = protocol_version, "Setting DSN protocol version...");

        let Self {
            listen_addresses,
            reserved_nodes,
            allow_non_global_addresses_in_dht,
            provider_storage_path: _,
            in_connections: InConnections(max_established_incoming_connections),
            out_connections: OutConnections(max_established_outgoing_connections),
            target_connections: TargetConnections(target_connections),
            pending_in_connections: PendingInConnections(max_pending_incoming_connections),
            pending_out_connections: PendingOutConnections(max_pending_outgoing_connections),
            boot_nodes,
            external_addresses,
        } = self;

        let bootstrap_nodes = boot_nodes.into_iter().map(Into::into).collect::<Vec<_>>();

        let listen_on = listen_addresses.0.into_iter().map(Into::into).collect();

        let networking_parameters_registry = subspace_networking::NetworkingParametersManager::new(
            &base_path.join("known_addresses.bin"),
            strip_peer_id(bootstrap_nodes.clone())
                .into_iter()
                .map(|(peer_id, _)| peer_id)
                .collect::<HashSet<_>>(),
        )
        .context("Failed to open known addresses database for DSN")?
        .boxed();

        let default_networking_config = subspace_networking::Config::new(
            protocol_version,
            keypair,
            local_records_provider.clone(),
            Some(PeerInfoProvider::new_farmer(Box::new(farmer_archival_storage_pieces.clone()))),
        );

        let config = subspace_networking::Config {
            listen_on,
            allow_non_global_addresses_in_dht,
            networking_parameters_registry: Some(networking_parameters_registry),
            request_response_protocols: vec![
                PieceByIndexRequestHandler::create({
                    let weak_readers_and_pieces = Arc::downgrade(&farmer_readers_and_pieces);
                    let farmer_piece_cache = farmer_piece_cache.clone();
                    move |_, req| {
                        let weak_readers_and_pieces = weak_readers_and_pieces.clone();
                        let farmer_piece_cache = farmer_piece_cache.clone();

                        get_piece_by_index(req, weak_readers_and_pieces, farmer_piece_cache)
                    }
                }),
                SegmentHeaderBySegmentIndexesRequestHandler::create({
                    let segment_header_store = segment_header_store.clone();
                    move |_, req| {
                        futures::future::ready(get_segment_header_by_segment_indexes(
                            req,
                            &segment_header_store,
                        ))
                    }
                }),
            ],
            reserved_peers: reserved_nodes.into_iter().map(Into::into).collect(),
            max_established_incoming_connections,
            max_established_outgoing_connections,
            general_target_connections: target_connections,
            // maintain permanent connections between farmers
            special_connected_peers_handler: Some(Arc::new(PeerInfo::is_farmer)),
            // other (non-farmer) connections
            general_connected_peers_handler: Some(Arc::new(|peer_info| {
                !PeerInfo::is_farmer(peer_info)
            })),
            max_pending_incoming_connections,
            max_pending_outgoing_connections,
            bootstrap_addresses: bootstrap_nodes,
            external_addresses: external_addresses.into_iter().map(Into::into).collect(),
            ..default_networking_config
        };

        let (node, runner) = subspace_networking::create(config)?;

        let mut destructors = DestructorSet::new_without_async("dsn-destructors");
        let on_new_listener = node.on_new_listener(Arc::new({
            let node = node.clone();

            move |address| {
                tracing::info!(
                    "DSN listening on {}",
                    address
                        .clone()
                        .with(subspace_networking::libp2p::multiaddr::Protocol::P2p(node.id()))
                );
            }
        }));
        destructors.add_items_to_drop(on_new_listener)?;

        let on_peer_info = node.on_peer_info(Arc::new({
            let archival_storage_info = farmer_archival_storage_info.clone();

            move |new_peer_info| {
                let peer_id = new_peer_info.peer_id;
                let peer_info = &new_peer_info.peer_info;

                if let PeerInfo::Farmer { cuckoo_filter } = peer_info {
                    archival_storage_info.update_cuckoo_filter(peer_id, cuckoo_filter.clone());

                    tracing::debug!(%peer_id, ?peer_info, "Peer info cached",);
                }
            }
        }));
        destructors.add_items_to_drop(on_peer_info)?;

        let on_disconnected_peer = node.on_disconnected_peer(Arc::new({
            let archival_storage_info = farmer_archival_storage_info.clone();

            move |peer_id| {
                if archival_storage_info.remove_peer_filter(peer_id) {
                    tracing::debug!(%peer_id, "Peer filter removed.",);
                }
            }
        }));
        destructors.add_items_to_drop(on_disconnected_peer)?;

        Ok((
            DsnShared {
                node,
                farmer_readers_and_pieces,
                _destructors: destructors,
                farmer_archival_storage_pieces,
                farmer_archival_storage_info,
                farmer_piece_cache,
            },
            runner,
        ))
    }
}

use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::{Arc, Weak};

use anyhow::Context;
use derivative::Derivative;
use derive_builder::Builder;
use derive_more::{Deref, DerefMut, Display, From};
use either::*;
use futures::prelude::*;
use sdk_utils::{self, DropCollection, Multiaddr, MultiaddrWithPeerId};
use serde::{Deserialize, Serialize};
use subspace_core_primitives::Piece;
use subspace_farmer::utils::archival_storage_info::ArchivalStorageInfo;
use subspace_farmer::utils::archival_storage_pieces::ArchivalStoragePieces;
use subspace_farmer::utils::readers_and_pieces::ReadersAndPieces;
use subspace_networking::libp2p::kad::ProviderRecord;
use subspace_networking::{
    PeerInfo, PeerInfoProvider, PieceAnnouncementRequestHandler, PieceAnnouncementResponse,
    PieceByHashRequest, PieceByHashRequestHandler, PieceByHashResponse, ProviderStorage as _,
    SegmentHeaderBySegmentIndexesRequestHandler, SegmentHeaderRequest, SegmentHeaderResponse,
    KADEMLIA_PROVIDER_TTL_IN_SECS,
};
use subspace_service::segment_headers::SegmentHeaderCache;
use subspace_service::Error;

use super::provider_storage_utils::MaybeProviderStorage;
use super::{FarmerProviderStorage, NodePieceCache, NodeProviderStorage, ProviderStorage};

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

const MAX_PROVIDER_RECORDS_LIMIT: NonZeroUsize = NonZeroUsize::new(100000).expect("100000 > 0"); // ~ 10 MB
const MAX_CONCURRENT_ANNOUNCEMENTS_QUEUE: NonZeroUsize =
    NonZeroUsize::new(2000).expect("Not zero; qed");

/// Options for DSN
pub struct DsnOptions<C, ASNS, PieceByHash, SegmentHeaderByIndexes> {
    /// Client to aux storage for node piece cache
    pub client: Arc<C>,
    /// Node telemetry name
    pub node_name: String,
    /// Archived segment notification stream
    pub archived_segment_notification_stream: ASNS,
    /// Node piece cache size
    pub piece_cache_size: sdk_utils::ByteSize,
    /// Path for dsn
    pub base_path: PathBuf,
    /// Keypair for networking
    pub keypair: subspace_networking::libp2p::identity::Keypair,
    /// Get piece by hash handler
    pub get_piece_by_hash: PieceByHash,
    /// Get segment header by segment indexes handler
    pub get_segment_header_by_segment_indexes: SegmentHeaderByIndexes,
    /// Farmer total allocated space across all plots
    pub farmer_total_space_pledged: usize,
}

/// Farmer piece store
pub type PieceStore = subspace_farmer::utils::parity_db_store::ParityDbStore<
    subspace_networking::libp2p::kad::record::Key,
    subspace_core_primitives::Piece,
>;

/// Shared Dsn structure between node and farmer
#[derive(Derivative)]
#[derivative(Debug)]
pub struct DsnShared<C: sc_client_api::AuxStore + Send + Sync + 'static> {
    /// Dsn node
    pub node: subspace_networking::Node,
    /// Farmer readers and pieces
    pub farmer_readers_and_pieces: Arc<parking_lot::Mutex<Option<ReadersAndPieces>>>,
    /// Farmer piece store
    #[derivative(Debug = "ignore")]
    pub farmer_piece_store: Arc<tokio::sync::Mutex<Option<PieceStore>>>,
    /// Provider records receiver
    pub provider_records_receiver:
        parking_lot::Mutex<Option<futures::channel::mpsc::Receiver<ProviderRecord>>>,
    /// Farmer provider storage
    pub farmer_provider_storage: MaybeProviderStorage<FarmerProviderStorage>,
    /// Farmer archival storage pieces
    pub farmer_archival_storage_pieces: ArchivalStoragePieces,
    /// Farmer archival storage info
    pub farmer_archival_storage_info: ArchivalStorageInfo,
    /// Farmer piece cache
    #[derivative(Debug = "ignore")]
    pub piece_cache: NodePieceCache<C>,
    _drop: DropCollection,
}

impl Dsn {
    /// Build dsn
    pub fn build_dsn<B, C, ASNS, PieceByHash, F1, SegmentHeaderByIndexes>(
        self,
        options: DsnOptions<C, ASNS, PieceByHash, SegmentHeaderByIndexes>,
    ) -> anyhow::Result<(DsnShared<C>, subspace_networking::NodeRunner<ProviderStorage<C>>)>
    where
        B: sp_runtime::traits::Block,
        C: sc_client_api::AuxStore + sp_blockchain::HeaderBackend<B> + Send + Sync + 'static,
        ASNS: Stream<Item = sc_consensus_subspace::ArchivedSegmentNotification>
            + Unpin
            + Send
            + 'static,
        PieceByHash: Fn(
                &PieceByHashRequest,
                Weak<parking_lot::Mutex<Option<ReadersAndPieces>>>,
                Arc<tokio::sync::Mutex<Option<PieceStore>>>,
                NodePieceCache<C>,
            ) -> F1
            + Send
            + Sync
            + 'static,
        F1: Future<Output = Option<PieceByHashResponse>> + Send + 'static,
        SegmentHeaderByIndexes: Fn(&SegmentHeaderRequest, &SegmentHeaderCache<C>) -> Option<SegmentHeaderResponse>
            + Send
            + Sync
            + 'static,
    {
        let DsnOptions {
            mut archived_segment_notification_stream,
            node_name,
            client,
            base_path,
            keypair,
            piece_cache_size,
            get_piece_by_hash,
            get_segment_header_by_segment_indexes,
            farmer_total_space_pledged,
        } = options;
        let farmer_readers_and_pieces = Arc::new(parking_lot::Mutex::new(None));
        let farmer_piece_store = Arc::new(tokio::sync::Mutex::new(None));
        let farmer_provider_storage = MaybeProviderStorage::none();
        let protocol_version = hex::encode(client.info().genesis_hash);

        let cuckoo_filter_size = farmer_total_space_pledged / Piece::SIZE + 1usize;
        let farmer_archival_storage_pieces = ArchivalStoragePieces::new(cuckoo_filter_size);
        let farmer_archival_storage_info = ArchivalStorageInfo::default();

        tracing::debug!(genesis_hash = protocol_version, "Setting DSN protocol version...");

        let piece_cache = NodePieceCache::new(
            client.clone(),
            piece_cache_size.as_u64(),
            subspace_networking::peer_id(&keypair),
        );

        // Start before archiver, so we don't have potential race condition and
        // miss pieces
        sdk_utils::task_spawn(format!("subspace-sdk-node-{node_name}-piece-caching"), {
            let mut piece_cache = piece_cache.clone();

            async move {
                while let Some(archived_segment_notification) =
                    archived_segment_notification_stream.next().await
                {
                    let segment_index = archived_segment_notification
                        .archived_segment
                        .segment_header
                        .segment_index();
                    if let Err(error) = piece_cache.add_pieces(
                        segment_index.first_piece_index(),
                        &archived_segment_notification.archived_segment.pieces,
                    ) {
                        tracing::error!(
                            %segment_index,
                            %error,
                            "Failed to store pieces for segment in cache"
                        );
                    }
                }
            }
        });

        let Self {
            listen_addresses,
            reserved_nodes,
            allow_non_global_addresses_in_dht,
            provider_storage_path,
            in_connections: InConnections(max_established_incoming_connections),
            out_connections: OutConnections(max_established_outgoing_connections),
            target_connections: TargetConnections(target_connections),
            pending_in_connections: PendingInConnections(max_pending_incoming_connections),
            pending_out_connections: PendingOutConnections(max_pending_outgoing_connections),
            boot_nodes,
        } = self;

        let peer_id = subspace_networking::peer_id(&keypair);
        let bootstrap_nodes = boot_nodes.into_iter().map(Into::into).collect::<Vec<_>>();

        let listen_on = listen_addresses.0.into_iter().map(Into::into).collect();

        let networking_parameters_registry = subspace_networking::NetworkingParametersManager::new(
            &base_path.join("known_addresses_db"),
            bootstrap_nodes,
        )
        .context("Failed to open known addresses database for DSN")?
        .boxed();

        let external_provider_storage = match provider_storage_path {
            Some(path) => Either::Left(subspace_networking::ParityDbProviderStorage::new(
                &path,
                MAX_PROVIDER_RECORDS_LIMIT,
                peer_id,
            )?),
            None => Either::Right(subspace_networking::MemoryProviderStorage::new(peer_id)),
        };

        let node_provider_storage =
            NodeProviderStorage::new(peer_id, piece_cache.clone(), external_provider_storage);
        let provider_storage =
            ProviderStorage::new(farmer_provider_storage.clone(), node_provider_storage);

        let (provider_records_sender, provider_records_receiver) =
            futures::channel::mpsc::channel(MAX_CONCURRENT_ANNOUNCEMENTS_QUEUE.get());

        let mut default_networking_config = subspace_networking::Config::new(
            protocol_version,
            keypair,
            provider_storage.clone(),
            PeerInfoProvider::new_farmer(Box::new(farmer_archival_storage_pieces.clone())),
        );
        default_networking_config.kademlia.set_provider_record_ttl(KADEMLIA_PROVIDER_TTL_IN_SECS);

        let config = subspace_networking::Config {
            listen_on,
            allow_non_global_addresses_in_dht,
            networking_parameters_registry,
            request_response_protocols: vec![
                PieceAnnouncementRequestHandler::create({
                    move |peer_id, req| {
                        tracing::trace!(?req, %peer_id, "Piece announcement request received.");

                        let mut provider_records_sender = provider_records_sender.clone();
                        let provider_record = ProviderRecord {
                            provider: peer_id,
                            key: req.piece_index_hash.into(),
                            addresses: req.addresses.clone(),
                            expires: KADEMLIA_PROVIDER_TTL_IN_SECS
                                .map(|ttl| std::time::Instant::now() + ttl),
                        };

                        let result = match provider_storage.add_provider(provider_record.clone()) {
                            Ok(()) => {
                                if let Err(error) =
                                    provider_records_sender.try_send(provider_record)
                                {
                                    if !error.is_disconnected() {
                                        let record = error.into_inner();
                                        // TODO: This should be made a warning, but due to
                                        //  https://github.com/libp2p/rust-libp2p/discussions/3411 it'll
                                        //  take us some time to resolve
                                        tracing::debug!(
                                            ?record.key,
                                            ?record.provider,
                                            "Failed to add provider record to the channel."
                                        );
                                    }
                                };

                                Some(PieceAnnouncementResponse::Success)
                            }
                            Err(error) => {
                                tracing::error!(
                                    %error,
                                    %peer_id,
                                    ?req,
                                    "Failed to add provider for received key."
                                );

                                None
                            }
                        };

                        futures::future::ready(result)
                    }
                }),
                PieceByHashRequestHandler::create({
                    let weak_readers_and_pieces = Arc::downgrade(&farmer_readers_and_pieces);
                    let farmer_piece_store = Arc::clone(&farmer_piece_store);
                    let piece_cache = piece_cache.clone();
                    move |_, req| {
                        let weak_readers_and_pieces = weak_readers_and_pieces.clone();
                        let farmer_piece_store = Arc::clone(&farmer_piece_store);
                        let piece_cache = piece_cache.clone();

                        get_piece_by_hash(
                            req,
                            weak_readers_and_pieces,
                            farmer_piece_store,
                            piece_cache,
                        )
                    }
                }),
                SegmentHeaderBySegmentIndexesRequestHandler::create({
                    let segment_header_cache =
                        SegmentHeaderCache::new(client).map_err(|error| {
                            Error::Other(
                                format!("Failed to instantiate segment header cache: {error}")
                                    .into(),
                            )
                        })?;
                    move |_, req| {
                        futures::future::ready(get_segment_header_by_segment_indexes(
                            req,
                            &segment_header_cache,
                        ))
                    }
                }),
            ],
            reserved_peers: reserved_nodes.into_iter().map(Into::into).collect(),
            max_established_incoming_connections,
            max_established_outgoing_connections,
            general_target_connections: target_connections,
            // maintain permanent connections between farmers
            special_connected_peers_handler: Arc::new(PeerInfo::is_farmer),
            // other (non-farmer) connections
            general_connected_peers_handler: Arc::new(|peer_info| !PeerInfo::is_farmer(peer_info)),
            max_pending_incoming_connections,
            max_pending_outgoing_connections,
            ..default_networking_config
        };

        let (node, runner) = subspace_networking::create(config)?;

        let mut drop_collection = DropCollection::new();
        let on_new_listener = node.on_new_listener(Arc::new({
            let node = node.clone();

            move |address| {
                tracing::info!(
                    "DSN listening on {}",
                    address.clone().with(subspace_networking::libp2p::multiaddr::Protocol::P2p(
                        node.id().into()
                    ))
                );
            }
        }));
        drop_collection.push(on_new_listener);

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
        drop_collection.push(on_peer_info);

        let on_disconnected_peer = node.on_disconnected_peer(Arc::new({
            let archival_storage_info = farmer_archival_storage_info.clone();

            move |peer_id| {
                if archival_storage_info.remove_peer_filter(peer_id) {
                    tracing::debug!(%peer_id, "Peer filter removed.",);
                }
            }
        }));
        drop_collection.push(on_disconnected_peer);

        Ok((
            DsnShared {
                node,
                farmer_piece_store,
                provider_records_receiver: parking_lot::Mutex::new(Some(provider_records_receiver)),
                farmer_provider_storage,
                farmer_readers_and_pieces,
                piece_cache,
                _drop: drop_collection,
                farmer_archival_storage_pieces,
                farmer_archival_storage_info,
            },
            runner,
        ))
    }
}

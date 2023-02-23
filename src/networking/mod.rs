pub(crate) mod provider_storage_utils;

use std::num::NonZeroUsize;
use std::sync::{Arc, Weak};

use either::*;
use event_listener_primitives::HandlerId;
use futures::StreamExt;
use parking_lot::Mutex;
use sc_client_api::AuxStore;
use subspace_core_primitives::PieceIndexHash;
use subspace_farmer::utils::farmer_provider_record_processor::FarmerProviderRecordProcessor;
use subspace_farmer::utils::readers_and_pieces::ReadersAndPieces;
use subspace_farmer_components::plotting::PieceGetter;
use subspace_networking::utils::piece_provider::PieceValidator;
use subspace_networking::{Node, ParityDbProviderStorage};
use tracing::{warn, Instrument};

pub(crate) type FarmerPieceCache = subspace_farmer::utils::farmer_piece_cache::FarmerPieceCache;
pub(crate) type NodePieceCache<C> = subspace_service::piece_cache::PieceCache<C>;
pub(crate) type PieceCache = FarmerPieceCache;
pub(crate) type FarmerProviderStorage =
    subspace_farmer::utils::farmer_provider_storage::FarmerProviderStorage<
        ParityDbProviderStorage,
        PieceCache,
    >;
pub(crate) type NodeProviderStorage<C> =
    subspace_service::dsn::node_provider_storage::NodeProviderStorage<
        NodePieceCache<C>,
        Either<ParityDbProviderStorage, subspace_networking::MemoryProviderStorage>,
    >;
pub(crate) type ProviderStorage<C> = provider_storage_utils::AndProviderStorage<
    provider_storage_utils::MaybeProviderStorage<FarmerProviderStorage>,
    NodeProviderStorage<C>,
>;

const MAX_CONCURRENT_ANNOUNCEMENTS_QUEUE: NonZeroUsize =
    NonZeroUsize::new(2000).expect("Not zero; qed");
const MAX_CONCURRENT_ANNOUNCEMENTS_PROCESSING: NonZeroUsize =
    NonZeroUsize::new(20).expect("Not zero; qed");
const MAX_CONCURRENT_RE_ANNOUNCEMENTS_PROCESSING: NonZeroUsize =
    NonZeroUsize::new(100).expect("Not zero; qed");

/// Start processing announcements received by the network node, returns handle
/// that will stop processing on drop.
pub fn start_announcements_processor(
    node: Node,
    piece_cache: Arc<tokio::sync::Mutex<FarmerPieceCache>>,
    weak_readers_and_pieces: Weak<Mutex<Option<ReadersAndPieces>>>,
) -> std::io::Result<HandlerId> {
    let (provider_records_sender, mut provider_records_receiver) =
        futures::channel::mpsc::channel(MAX_CONCURRENT_ANNOUNCEMENTS_QUEUE.get());

    let handler_id = node.on_announcement(Arc::new({
        let provider_records_sender = Mutex::new(provider_records_sender);

        move |record, guard| {
            if let Err(error) =
                provider_records_sender.lock().try_send((record.clone(), Arc::clone(guard)))
            {
                if error.is_disconnected() {
                    // Receiver exited, nothing left to be done
                    return;
                }
                let (record, _guard) = error.into_inner();
                warn!(
                    ?record.key,
                    ?record.provider,
                    "Failed to add provider record to the channel."
                );
            };
        }
    }));

    let handle = tokio::runtime::Handle::current();
    let span = tracing::Span::current();
    let mut provider_record_processor = FarmerProviderRecordProcessor::new(
        node,
        piece_cache,
        weak_readers_and_pieces,
        MAX_CONCURRENT_ANNOUNCEMENTS_PROCESSING,
        MAX_CONCURRENT_RE_ANNOUNCEMENTS_PROCESSING,
    );

    // We are working with database internally, better to run in a separate thread
    std::thread::Builder::new().name("ann-processor".to_string()).spawn(move || {
        let processor_fut = async {
            while let Some((provider_record, guard)) = provider_records_receiver.next().await {
                provider_record_processor.process_provider_record(provider_record, guard).await;
            }
        };

        handle.block_on(processor_fut.instrument(span));
    })?;

    Ok(handler_id)
}

pub(crate) struct NodePieceGetter<PV, C> {
    piece_getter: subspace_farmer::utils::node_piece_getter::NodePieceGetter<PV>,
    node_cache: NodePieceCache<C>,
}

impl<PV, C> NodePieceGetter<PV, C> {
    pub fn new(
        dsn_piece_getter: subspace_farmer::utils::node_piece_getter::NodePieceGetter<PV>,
        node_cache: NodePieceCache<C>,
    ) -> Self {
        Self { piece_getter: dsn_piece_getter, node_cache }
    }
}

#[async_trait::async_trait()]
impl<PV: PieceValidator, C: AuxStore + Send + Sync> PieceGetter for NodePieceGetter<PV, C> {
    async fn get_piece(
        &self,
        piece_index: subspace_core_primitives::PieceIndex,
    ) -> Result<
        Option<subspace_core_primitives::Piece>,
        Box<dyn std::error::Error + Send + Sync + 'static>,
    > {
        let piece = self
            .node_cache
            .get_piece(PieceIndexHash::from_index(piece_index))
            .map_err(|x| x.to_string())?;
        if piece.is_some() {
            return Ok(piece);
        }

        self.piece_getter.get_piece(piece_index).await
    }
}

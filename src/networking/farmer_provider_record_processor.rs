use std::num::NonZeroUsize;
use std::sync::{Arc, Weak};

use event_listener_primitives::HandlerId;
use futures::StreamExt;
use parking_lot::Mutex;
use subspace_farmer::utils::farmer_piece_cache::FarmerPieceCache;
use subspace_farmer::utils::farmer_provider_record_processor::FarmerProviderRecordProcessor;
use subspace_farmer::utils::readers_and_pieces::ReadersAndPieces;
use subspace_networking::Node;
use tracing::{warn, Instrument};

const MAX_CONCURRENT_ANNOUNCEMENTS_QUEUE: usize = 2000;
const MAX_CONCURRENT_ANNOUNCEMENTS_PROCESSING: NonZeroUsize =
    NonZeroUsize::new(20).expect("Not zero; qed");

/// Start processing announcements received by the network node, returns handle
/// that will stop processing on drop.
pub fn start_announcements_processor(
    node: Node,
    piece_cache: Arc<tokio::sync::Mutex<FarmerPieceCache>>,
    weak_readers_and_pieces: Weak<Mutex<Option<ReadersAndPieces>>>,
) -> std::io::Result<HandlerId> {
    let (provider_records_sender, mut provider_records_receiver) =
        futures::channel::mpsc::channel(MAX_CONCURRENT_ANNOUNCEMENTS_QUEUE);

    let handler_id = node.on_announcement(Arc::new({
        let provider_records_sender = Mutex::new(provider_records_sender);

        move |record| {
            if let Err(error) = provider_records_sender.lock().try_send(record.clone()) {
                if error.is_disconnected() {
                    // Receiver exited, nothing left to be done
                    return;
                }
                let record = error.into_inner();
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
        weak_readers_and_pieces.clone(),
        MAX_CONCURRENT_ANNOUNCEMENTS_PROCESSING,
    );

    // We are working with database internally, better to run in a separate thread
    std::thread::Builder::new().name("ann-processor".to_string()).spawn(move || {
        let processor_fut = async {
            while let Some(provider_record) = provider_records_receiver.next().await {
                provider_record_processor.process_provider_record(provider_record).await;
            }
        };

        handle.block_on(processor_fut.instrument(span));
    })?;

    Ok(handler_id)
}

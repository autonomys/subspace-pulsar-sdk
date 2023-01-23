use std::num::NonZeroUsize;
use std::sync::Arc;

use event_listener_primitives::HandlerId;
use futures::StreamExt;
use parking_lot::Mutex;
use subspace_core_primitives::{Blake2b256Hash, Piece, PieceIndexHash, BLAKE2B_256_HASH_SIZE};
use subspace_networking::libp2p::kad::ProviderRecord;
use subspace_networking::libp2p::multihash::Multihash;
use subspace_networking::libp2p::PeerId;
use subspace_networking::utils::multihash::MultihashCode;
use subspace_networking::{Node, PieceByHashRequest, PieceByHashResponse};
use tokio::sync::Semaphore;
use tracing::{debug, trace, warn, Instrument};

use super::farmer_piece_storage::LimitedSizeParityDbStore;
use super::PieceStorage;

const MAX_CONCURRENT_ANNOUNCEMENTS_QUEUE: usize = 2000;
const MAX_CONCURRENT_ANNOUNCEMENTS_PROCESSING: NonZeroUsize =
    NonZeroUsize::new(20).expect("Not zero; qed");

/// Start processing announcements received by the network node, returns handle
/// that will stop processing on drop.
pub fn start_announcements_processor(
    node: Node,
    piece_storage: LimitedSizeParityDbStore,
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
        piece_storage,
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

// TODO: This should probably moved into the library
pub(crate) struct FarmerProviderRecordProcessor<PS> {
    node: Node,
    piece_storage: Arc<tokio::sync::Mutex<PS>>,
    semaphore: Arc<Semaphore>,
}

impl<PS> FarmerProviderRecordProcessor<PS>
where
    PS: PieceStorage + Send + 'static,
{
    pub fn new(node: Node, piece_storage: PS, max_concurrent_announcements: NonZeroUsize) -> Self {
        let semaphore = Arc::new(Semaphore::new(max_concurrent_announcements.get()));
        Self { node, piece_storage: Arc::new(tokio::sync::Mutex::new(piece_storage)), semaphore }
    }

    pub async fn process_provider_record(&mut self, provider_record: ProviderRecord) {
        trace!(?provider_record.key, "Starting processing provider record...");

        let multihash = match Multihash::from_bytes(provider_record.key.as_ref()) {
            Ok(multihash) => multihash,
            Err(error) => {
                trace!(
                    ?provider_record.key,
                    %error,
                    "Record is not a correct multihash, ignoring"
                );
                return;
            }
        };

        if multihash.code() != u64::from(MultihashCode::PieceIndexHash) {
            trace!(
                ?provider_record.key,
                code = %multihash.code(),
                "Record is not a piece, ignoring"
            );
            return;
        }

        let piece_index_hash =
            Blake2b256Hash::try_from(&multihash.digest()[..BLAKE2B_256_HASH_SIZE])
                .expect(
                    "Multihash has 64-byte digest, which is sufficient for 32-byte Blake2b hash; \
                     qed",
                )
                .into();

        let Ok(permit) = self.semaphore.clone().acquire_owned().await else {
            return;
        };

        let node = self.node.clone();
        let piece_storage = Arc::clone(&self.piece_storage);

        tokio::spawn(async move {
            {
                let piece_storage = piece_storage.lock().await;

                if !piece_storage.should_include_in_storage(&provider_record.key) {
                    return;
                }

                if piece_storage.get_piece(&provider_record.key).is_some() {
                    trace!(key=?provider_record.key, "Skipped processing local piece...");
                    return;
                }

                // TODO: Store local intent to cache a piece such that we don't
                // try to pull the same piece again
            }

            if let Some(piece) =
                get_piece_from_announcer(&node, piece_index_hash, provider_record.provider).await
            {
                piece_storage.lock().await.add_piece(provider_record.key.clone(), piece);
                if let Err(error) =
                    subspace_networking::utils::pieces::announce_single_piece_index_hash_with_backoff(piece_index_hash, &node).await
                {
                    debug!(
                        ?error,
                        ?piece_index_hash,
                        "Announcing cached piece index hash failed"
                    );
                }
            }

            drop(permit);
        });
    }
}

async fn get_piece_from_announcer(
    node: &Node,
    piece_index_hash: PieceIndexHash,
    provider: PeerId,
) -> Option<Piece> {
    let request_result =
        node.send_generic_request(provider, PieceByHashRequest { piece_index_hash }).await;

    // TODO: Nothing guarantees that piece index hash is real, response must also
    // return piece index  that matches piece index hash and piece must be
    // verified against blockchain after that
    match request_result {
        Ok(PieceByHashResponse { piece: Some(piece) }) => {
            trace!(
                %provider,
                ?piece_index_hash,
                "Piece request succeeded."
            );

            return Some(piece);
        }
        Ok(PieceByHashResponse { piece: None }) => {
            debug!(
                %provider,
                ?piece_index_hash,
                "Provider returned no piece right after announcement."
            );
        }
        Err(error) => {
            warn!(
                %provider,
                ?piece_index_hash,
                ?error,
                "Piece request to announcer provider failed."
            );
        }
    }

    None
}

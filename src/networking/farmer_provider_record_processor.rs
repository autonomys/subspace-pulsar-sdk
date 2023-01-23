use std::num::NonZeroUsize;
use std::sync::Arc;

use futures::StreamExt;
use subspace_core_primitives::{Blake2b256Hash, Piece, PieceIndexHash, BLAKE2B_256_HASH_SIZE};
use subspace_networking::libp2p::kad::record::Key;
use subspace_networking::libp2p::kad::ProviderRecord;
use subspace_networking::libp2p::multihash::Multihash;
use subspace_networking::libp2p::PeerId;
use subspace_networking::utils::multihash::MultihashCode;
use subspace_networking::{Node, PieceByHashRequest, PieceByHashResponse};
use tokio::sync::Semaphore;
use tracing::{debug, trace, warn};

use super::PieceStorage;

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
                announce_piece(&node, provider_record.key).await;
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

//TODO: consider introducing publish-piece helper
async fn announce_piece(node: &Node, key: Key) {
    let result = node.start_announcing(key.clone()).await;

    match result {
        Err(error) => {
            debug!(?error, ?key, "Piece publishing for the cache returned an error");
        }
        Ok(mut stream) =>
            if stream.next().await.is_some() {
                trace!(?key, "Piece publishing for the cache succeeded");
            } else {
                debug!(?key, "Piece publishing for the cache failed");
            },
    };
}

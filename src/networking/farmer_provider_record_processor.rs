use futures::StreamExt;
use subspace_core_primitives::{Blake2b256Hash, Piece, PieceIndexHash, BLAKE2B_256_HASH_SIZE};
use subspace_networking::libp2p::kad::ProviderRecord;
use subspace_networking::libp2p::multihash::Multihash;
use subspace_networking::{Node, PieceByHashRequest, PieceByHashResponse, ToMultihash};
use tracing::{debug, trace, warn};

use super::PieceStorage;

pub(crate) struct FarmerProviderRecordProcessor<PS: PieceStorage> {
    node: Node,
    piece_storage: PS,
}

impl<PS: PieceStorage> FarmerProviderRecordProcessor<PS> {
    pub fn new(node: Node, piece_storage: PS) -> Self {
        Self { node, piece_storage }
    }

    //TODO: consider introducing get-piece helper
    async fn get_piece(&self, piece_index_hash: PieceIndexHash) -> Option<Piece> {
        let multihash = piece_index_hash.to_multihash();

        let get_providers_result = self.node.get_providers(multihash).await;

        match get_providers_result {
            Ok(mut get_providers_stream) => {
                while let Some(provider_id) = get_providers_stream.next().await {
                    trace!(?multihash, %provider_id, "get_providers returned an item");

                    if provider_id == self.node.id() {
                        trace!(?multihash, %provider_id, "Attempted to get a piece from itself.");
                        continue;
                    }

                    let request_result = self
                        .node
                        .send_generic_request(provider_id, PieceByHashRequest { piece_index_hash })
                        .await;

                    match request_result {
                        Ok(PieceByHashResponse { piece: Some(piece) }) => {
                            trace!(%provider_id, ?multihash, ?piece_index_hash, "Piece request succeeded.");
                            return Some(piece);
                        }
                        Ok(PieceByHashResponse { piece: None }) => {
                            debug!(%provider_id, ?multihash, ?piece_index_hash, "Piece request returned empty piece.");
                        }
                        Err(error) => {
                            warn!(%provider_id, ?multihash, ?piece_index_hash, ?error, "Piece request failed.");
                        }
                    }
                }
            }
            Err(err) => {
                warn!(?multihash, ?piece_index_hash, ?err, "get_providers returned an error");
            }
        }

        None
    }

    //TODO: consider introducing publish-piece helper
    async fn announce_piece(&self, key: Multihash) {
        let result = self.node.start_announcing(key).await;

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

    pub async fn process_provider_record(&mut self, rec: ProviderRecord) {
        let multihash_bytes = rec.key.to_vec();
        let multihash = Multihash::from_bytes(multihash_bytes.as_slice())
            .expect("Key should represent a valid multihash");

        if self.piece_storage.get_piece(&rec.key).is_some() {
            trace!(key=?rec.key, "Skipped processing local piece...");
            return;
        }

        trace!(key=?rec.key, "Starting processing provider record...");

        if self.piece_storage.should_include_in_storage(&rec.key) {
            let piece_index_hash: Blake2b256Hash = multihash.digest()[..BLAKE2B_256_HASH_SIZE]
                .try_into()
                .expect("Multihash should be known 32 bytes size.");

            if let Some(piece) = self.get_piece(piece_index_hash.into()).await {
                self.piece_storage.add_piece(rec.key.clone(), piece);
                self.announce_piece(multihash).await;
            }
        }
    }
}

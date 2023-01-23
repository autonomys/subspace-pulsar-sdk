use std::error::Error;
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use subspace_core_primitives::{Piece, PieceIndex, PieceIndexHash};
use subspace_farmer_components::plotting::PieceReceiver;
use subspace_networking::{Node, PieceProvider, ToMultihash};

use super::PieceStorage;

pub struct FarmerPieceReceiver<PV, PS> {
    piece_provider: PieceProvider<PV>,
    piece_storage: Arc<tokio::sync::Mutex<PS>>,
    node: Node,
}

impl<PV, PS> FarmerPieceReceiver<PV, PS> {
    pub fn new(
        piece_provider: PieceProvider<PV>,
        piece_storage: Arc<tokio::sync::Mutex<PS>>,
        node: Node,
    ) -> Self {
        Self { piece_provider, piece_storage, node }
    }
}

#[async_trait]
impl<PV, PS> PieceReceiver for FarmerPieceReceiver<PV, PS>
where
    PV: subspace_networking::PieceValidator,
    PS: PieceStorage + Send + 'static,
{
    async fn get_piece(
        &self,
        piece_index: PieceIndex,
    ) -> Result<Option<Piece>, Box<dyn Error + Send + Sync + 'static>> {
        let piece_index_hash = PieceIndexHash::from_index(piece_index);
        let key = piece_index_hash.to_multihash().into();

        let maybe_should_store = {
            let piece_storage = self.piece_storage.lock().await;
            if let Some(piece) = piece_storage.get_piece(&key) {
                return Ok(Some(piece));
            }

            piece_storage.should_include_in_storage(&key)
        };

        let maybe_piece = self.piece_provider.get_piece(piece_index).await?;

        if let Some(piece) = &maybe_piece {
            if maybe_should_store {
                let mut piece_storage = self.piece_storage.lock().await;
                if piece_storage.should_include_in_storage(&key)
                    && piece_storage.get_piece(&key).is_none()
                {
                    piece_storage.add_piece(key, piece.clone());
                    if let Err(error) =
                        subspace_networking::utils::pieces::announce_single_piece_index_hash_with_backoff(piece_index_hash, &self.node)
                            .await
                    {
                        tracing::debug!(
                            ?error,
                            ?piece_index_hash,
                            "Announcing retrieved and cached piece index hash failed"
                        );
                    }
                }
            }
        }

        Ok(maybe_piece)
    }
}

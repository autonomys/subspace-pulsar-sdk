use std::pin::Pin;

use futures::prelude::*;
use sc_consensus_subspace_rpc::SubspaceRpcApiClient;
use subspace_core_primitives::{Piece, PieceIndex, SegmentHeader, SegmentIndex};
use subspace_farmer::node_client::{Error, NodeClient, NodeClientExt};
use subspace_rpc_primitives::{
    FarmerAppInfo, NodeSyncStatus, RewardSignatureResponse, RewardSigningInfo, SlotInfo,
    SolutionResponse,
};

#[async_trait::async_trait]
impl NodeClientExt for crate::Rpc {
    async fn last_segment_headers(&self, limit: u64) -> Result<Vec<Option<SegmentHeader>>, Error> {
        Ok(SubspaceRpcApiClient::last_segment_headers(self, limit).await?)
    }
}

#[async_trait::async_trait]
impl NodeClient for crate::Rpc {
    async fn farmer_app_info(&self) -> Result<FarmerAppInfo, Error> {
        Ok(self.get_farmer_app_info().await?)
    }

    async fn subscribe_slot_info(
        &self,
    ) -> Result<Pin<Box<dyn Stream<Item = SlotInfo> + Send + 'static>>, Error> {
        Ok(Box::pin(
            SubspaceRpcApiClient::subscribe_slot_info(self)
                .await?
                .filter_map(|result| futures::future::ready(result.ok())),
        ))
    }

    async fn submit_solution_response(
        &self,
        solution_response: SolutionResponse,
    ) -> Result<(), Error> {
        Ok(SubspaceRpcApiClient::submit_solution_response(self, solution_response).await?)
    }

    async fn subscribe_reward_signing(
        &self,
    ) -> Result<Pin<Box<dyn Stream<Item = RewardSigningInfo> + Send + 'static>>, Error> {
        Ok(Box::pin(
            SubspaceRpcApiClient::subscribe_reward_signing(self)
                .await?
                .filter_map(|result| futures::future::ready(result.ok())),
        ))
    }

    async fn submit_reward_signature(
        &self,
        reward_signature: RewardSignatureResponse,
    ) -> Result<(), Error> {
        Ok(SubspaceRpcApiClient::submit_reward_signature(self, reward_signature).await?)
    }

    async fn subscribe_archived_segment_headers(
        &self,
    ) -> Result<Pin<Box<dyn Stream<Item = SegmentHeader> + Send + 'static>>, Error> {
        Ok(Box::pin(
            SubspaceRpcApiClient::subscribe_archived_segment_header(self)
                .await?
                .filter_map(|result| futures::future::ready(result.ok())),
        ))
    }

    async fn subscribe_node_sync_status_change(
        &self,
    ) -> Result<Pin<Box<dyn Stream<Item = NodeSyncStatus> + Send + 'static>>, Error> {
        Ok(Box::pin(
            SubspaceRpcApiClient::subscribe_node_sync_status_change(self)
                .await?
                .filter_map(|result| futures::future::ready(result.ok())),
        ))
    }

    async fn segment_headers(
        &self,
        segment_indexes: Vec<SegmentIndex>,
    ) -> Result<Vec<Option<SegmentHeader>>, Error> {
        Ok(SubspaceRpcApiClient::segment_headers(self, segment_indexes).await?)
    }

    async fn piece(&self, piece_index: PieceIndex) -> Result<Option<Piece>, Error> {
        let result = SubspaceRpcApiClient::piece(self, piece_index).await?;

        if let Some(bytes) = result {
            let piece = Piece::try_from(bytes.as_slice())
                .map_err(|_| format!("Cannot convert piece. PieceIndex={}", piece_index))?;

            return Ok(Some(piece));
        }

        Ok(None)
    }

    async fn acknowledge_archived_segment_header(
        &self,
        segment_index: SegmentIndex,
    ) -> Result<(), Error> {
        Ok(SubspaceRpcApiClient::acknowledge_archived_segment_header(self, segment_index).await?)
    }
}

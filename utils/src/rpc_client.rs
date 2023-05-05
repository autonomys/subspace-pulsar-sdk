use std::pin::Pin;

use futures::prelude::*;
use sc_consensus_subspace_rpc::SubspaceRpcApiClient;
use subspace_archiving::archiver::NewArchivedSegment;
use subspace_core_primitives::{SegmentCommitment, SegmentHeader, SegmentIndex};
use subspace_farmer::node_client::{Error, NodeClient};
use subspace_rpc_primitives::{
    FarmerAppInfo, RewardSignatureResponse, RewardSigningInfo, SlotInfo, SolutionResponse,
};

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

    async fn subscribe_archived_segments(
        &self,
    ) -> Result<Pin<Box<dyn Stream<Item = NewArchivedSegment> + Send + 'static>>, Error> {
        Ok(Box::pin(
            SubspaceRpcApiClient::subscribe_archived_segment(self)
                .await?
                .filter_map(|result| futures::future::ready(result.ok())),
        ))
    }

    async fn segment_commitments(
        &self,
        segment_indexes: Vec<SegmentIndex>,
    ) -> Result<Vec<Option<SegmentCommitment>>, Error> {
        Ok(SubspaceRpcApiClient::segment_commitments(self, segment_indexes).await?)
    }

    async fn segment_headers(
        &self,
        segment_indexes: Vec<SegmentIndex>,
    ) -> Result<Vec<Option<SegmentHeader>>, Error> {
        Ok(SubspaceRpcApiClient::segment_headers(self, segment_indexes).await?)
    }
}

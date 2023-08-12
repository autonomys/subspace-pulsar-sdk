use std::sync::Arc;

use derivative::Derivative;
use sc_service::RpcHandlers;
use sdk_utils::DestructorSet;

/// Progress of Domain
#[derive(Derivative)]
#[derivative(Debug)]
pub enum DomainBuildingProgress {
    Default,
    BuildingStarted,
    Bootstrapped,
    PreparingToStart,
    Starting,
}

/// Domain structure
#[derive(Derivative)]
#[derivative(Debug)]
#[must_use = "Domain should be closed"]
pub struct Domain {
    pub _destructors: DestructorSet,
    #[derivative(Debug = "ignore")]
    pub rpc_handlers: Arc<tokio::sync::RwLock<Option<RpcHandlers>>>,
    pub current_building_progress: Arc<tokio::sync::RwLock<DomainBuildingProgress>>,
    pub domain_runner_result_receiver: tokio::sync::oneshot::Receiver<anyhow::Result<()>>,
}

impl Domain {
    pub async fn close(self) -> anyhow::Result<()> {
        self._destructors.async_drop().await?;
        self.domain_runner_result_receiver.await?
    }
}

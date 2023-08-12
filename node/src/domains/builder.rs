use std::path::Path;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use derivative::Derivative;
use derive_builder::Builder;
use domain_client_operator::{BootstrapResult, Bootstrapper};
use domain_runtime_primitives::opaque::Block as DomainBlock;
use futures::future;
use futures::future::Either::{Left, Right};
use sc_consensus_subspace::notification::SubspaceNotificationStream;
use sc_consensus_subspace::{BlockImportingNotification, NewSlotNotification};
use sdk_substrate::{Base, BaseBuilder};
use serde::{Deserialize, Serialize};
use sp_domains::DomainId;
use sp_runtime::traits::Block as BlockT;
use subspace_runtime::RuntimeApi as CRuntimeApi;
use subspace_runtime_primitives::opaque::Block as CBlock;
use subspace_service::{FullClient as CFullClient, FullSelectChain};
use tokio::sync::oneshot;

use crate::domains::domain_instance_starter::DomainInstanceStarter;
use crate::domains::evm_chain_spec;
use crate::ExecutorDispatch as CExecutorDispatch;

#[derive(Debug, Clone, Derivative, Builder, Deserialize, Serialize, PartialEq)]
#[derivative(Default(bound = ""))]
#[builder(pattern = "owned", build_fn(private, name = "_build"), name = "Builder")]
#[non_exhaustive]
pub struct DomainConfig {
    #[builder(setter(into), default)]
    #[serde(default, skip_serializing_if = "sdk_utils::is_default")]
    pub chain_id: String,

    #[builder(setter(into), default)]
    #[serde(default, skip_serializing_if = "sdk_utils::is_default")]
    pub domain_id: DomainId,

    #[builder(setter(into), default)]
    #[serde(default, skip_serializing_if = "sdk_utils::is_default")]
    pub relayer_id: String,

    #[builder(setter(into), default)]
    #[serde(default, skip_serializing_if = "sdk_utils::is_default")]
    pub additional_args: Vec<String>,

    #[doc(hidden)]
    #[builder(
        setter(into, strip_option),
        field(type = "BaseBuilder", build = "self.base.build()")
    )]
    #[serde(flatten, skip_serializing_if = "sdk_utils::is_default")]
    pub base: Base,
}

sdk_substrate::derive_base!(@ Base => Builder);

impl DomainConfig {
    pub async fn run(
        self,
        directory: impl AsRef<Path> + Send,
        mut drop_receiver: oneshot::Receiver<Result<()>>,
        domain_worker_result_sender: oneshot::Sender<Result<()>>,
        consensus_client: Arc<CFullClient<CRuntimeApi, CExecutorDispatch>>,
        block_importing_notification_stream: SubspaceNotificationStream<
            BlockImportingNotification<CBlock>,
        >,
        new_slot_notification_stream: SubspaceNotificationStream<NewSlotNotification>,
        consensus_network_service: Arc<
            sc_network::NetworkService<CBlock, <CBlock as BlockT>::Hash>,
        >,
        consensus_sync_service: Arc<sc_network_sync::SyncingService<CBlock>>,
        select_chain: FullSelectChain,
    ) {
        let bootstrapper = Bootstrapper::<DomainBlock, _, _>::new(consensus_client.clone());

        let bootstrap_result = match future::select(
            Box::pin(bootstrapper.fetch_domain_bootstrap_info(self.domain_id)),
            &mut drop_receiver,
        )
        .await
        {
            Left((result, _)) => {
                if result.is_err() {
                    let err =
                        result.err().expect("Already checked that variant is an Err above; qed");
                    let _ = domain_worker_result_sender
                        .send(Err(anyhow!("Error while boostrapping the domain: {:?}", err)));
                    return;
                }
                result.expect("already checked for non-error value above")
            }
            Right(_) => {
                tracing::info!(
                    "Received drop signal while bootstrapping the domain with domain_id: {:?}. \
                     exiting...",
                    self.domain_id
                );
                let _ = domain_worker_result_sender.send(Ok(()));
                return;
            }
        };

        let BootstrapResult {
            domain_instance_data,
            domain_created_at,
            imported_block_notification_stream,
        } = bootstrap_result;

        let domain_spec_result = evm_chain_spec::create_domain_spec(
            self.domain_id,
            self.chain_id.as_str(),
            domain_instance_data,
        );

        let domain_spec = if domain_spec_result.is_err() {
            let err = domain_spec_result
                .err()
                .expect("Already checked that variant is an Err above; qed");
            let _ = domain_worker_result_sender
                .send(Err(anyhow!("Error while creating domain spec for the domain: {:?}", err)));
            return;
        } else {
            domain_spec_result.expect("already checked for error above; qed")
        };

        let domains_directory = directory.as_ref().join(format!(
            "domain-{}",
            <DomainId as Into<u32>>::into(self.domain_id)
        ));
        let service_config = self.base.configuration(domains_directory, domain_spec).await;

        let domain_starter = DomainInstanceStarter {
            service_config,
            domain_id: self.domain_id,
            relayer_id: self.relayer_id.clone(),
            runtime_type: Default::default(),
            additional_arguments: self.additional_args.clone(),
            consensus_client,
            block_importing_notification_stream,
            new_slot_notification_stream,
            consensus_network_service,
            consensus_sync_service,
            select_chain,
        };

        match future::select(
            Box::pin(domain_starter.start(domain_created_at, imported_block_notification_stream)),
            &mut drop_receiver,
        )
        .await
        {
            Left((result, _)) => {
                tracing::info!(
                    "Domain starter for domain id: {:?} returned with result: {:?}. \
                     exiting...",
                    self.domain_id,
                    result
                );
                let _ = domain_worker_result_sender.send(result);
            }
            Right(_) => {
                tracing::info!(
                    "Received drop signal while running the domain with domain_id: {:?}. \
                     exiting...",
                    self.domain_id
                );
                let _ = domain_worker_result_sender.send(Ok(()));
            }
        };
    }
}

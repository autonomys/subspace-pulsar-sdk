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
use sdk_utils::chain_spec::get_account_id_from_seed;
use sdk_utils::DestructorSet;
use serde::{Deserialize, Serialize};
use sp_core::crypto::AccountId32;
use sp_domains::DomainId;
use sp_runtime::traits::Block as BlockT;
use subspace_runtime::RuntimeApi as CRuntimeApi;
use subspace_runtime_primitives::opaque::Block as CBlock;
use subspace_service::{FullClient as CFullClient, FullSelectChain};
use tokio::sync::{oneshot, RwLock};

use crate::domains::domain::{Domain, DomainBuildingProgress};
use crate::domains::domain_instance_starter::DomainInstanceStarter;
use crate::domains::evm_chain_spec;
use crate::ExecutorDispatch as CExecutorDispatch;

/// Link to the consensus node
pub struct ConsensusNodeLink {
    /// Consensus client
    pub consensus_client: Arc<CFullClient<CRuntimeApi, CExecutorDispatch>>,
    /// Block import notification stream for consensus chain
    pub block_importing_notification_stream:
        SubspaceNotificationStream<BlockImportingNotification<CBlock>>,
    /// New slot notification stream for consensus chain
    pub new_slot_notification_stream: SubspaceNotificationStream<NewSlotNotification>,
    /// Reference to the consensus node network service
    pub consensus_network_service:
        Arc<sc_network::NetworkService<CBlock, <CBlock as BlockT>::Hash>>,
    /// Reference to the consensus node's network sync service
    pub consensus_sync_service: Arc<sc_network_sync::SyncingService<CBlock>>,
    /// Consensus node's select chain
    pub select_chain: FullSelectChain,
}

/// Domain node configuration
#[derive(Debug, Clone, Derivative, Builder, Deserialize, Serialize, PartialEq)]
#[builder(pattern = "owned", build_fn(private, name = "_build"))]
#[non_exhaustive]
pub struct DomainConfig {
    /// Chain ID of domain node (must be same as the consensus node's chain id)
    #[builder(setter(into), default)]
    #[serde(default, skip_serializing_if = "sdk_utils::is_default")]
    pub chain_id: String,

    /// Uniquely identifies a domain
    #[builder(setter(into), default)]
    #[serde(default, skip_serializing_if = "sdk_utils::is_default")]
    pub domain_id: DomainId,

    /// XDM Relayer's account id
    #[builder(setter(into))]
    pub relayer_id: AccountId32,

    /// Additional arguments to pass to domain instance starter
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

impl Default for DomainConfig {
    fn default() -> Self {
        DomainConfig {
            chain_id: "".to_string(),
            domain_id: Default::default(),
            relayer_id: get_account_id_from_seed("Alice"),
            additional_args: vec![],
            base: Default::default(),
        }
    }
}

sdk_substrate::derive_base!(@ Base => DomainConfigBuilder);

impl DomainConfig {
    /// Dev configuraiton
    pub fn dev() -> DomainConfigBuilder {
        DomainConfigBuilder::dev()
    }

    /// Gemini 3e configuraiton
    pub fn gemini_3e() -> DomainConfigBuilder {
        DomainConfigBuilder::gemini_3e()
    }

    /// Devnet configuraiton
    pub fn devnet() -> DomainConfigBuilder {
        DomainConfigBuilder::devnet()
    }
}

impl DomainConfigBuilder {
    /// New builder
    pub fn new() -> Self {
        Self::default()
    }

    /// Dev chain configuration
    pub fn dev() -> Self {
        Self::new()
            .chain_id("dev")
            .domain_id(DomainId::new(0))
            .relayer_id(get_account_id_from_seed("Alice"))
    }

    /// Gemini 3e configuration
    pub fn gemini_3e() -> Self {
        Self::new().chain_id("gemini-3e")
    }

    /// Devnet chain configuration
    pub fn devnet() -> Self {
        Self::new().chain_id("devnet")
    }

    /// Get configuration for saving on disk
    pub fn configuration(self) -> DomainConfig {
        self._build().expect("Build is infallible")
    }

    /// Build a domain node
    pub async fn build(
        self,
        directory: impl AsRef<Path> + Send + 'static,
        consensus_node_link: ConsensusNodeLink,
    ) -> Result<Domain> {
        self.configuration().build(directory, consensus_node_link).await
    }
}

impl DomainConfig {
    /// Build a domain node
    pub async fn build(
        self,
        directory: impl AsRef<Path> + Send + 'static,
        consensus_node_link: ConsensusNodeLink,
    ) -> Result<Domain> {
        let ConsensusNodeLink {
            consensus_client,
            block_importing_notification_stream,
            new_slot_notification_stream,
            consensus_network_service,
            consensus_sync_service,
            select_chain,
        } = consensus_node_link;
        let printable_domain_id: u32 = self.domain_id.into();
        let mut destructor_set =
            DestructorSet::new(format!("domain-{}-worker-destructor", printable_domain_id));
        let shared_rpc_handler = Arc::new(RwLock::new(None));
        let shared_progress_data = Arc::new(RwLock::new(DomainBuildingProgress::Default));

        let (bootstrapping_result_sender, bootstrapping_result_receiver) = oneshot::channel();
        let (bootstrapping_worker_drop_sender, bootstrapping_worker_drop_receiver) =
            oneshot::channel();
        let domain_bootstrapper_join_handle = sdk_utils::task_spawn(
            format!("domain/domain-{}/bootstrapping", printable_domain_id),
            {
                let consensus_client = consensus_client.clone();
                let shared_progress_data = shared_progress_data.clone();
                async move {
                    *shared_progress_data.write().await = DomainBuildingProgress::BuildingStarted;
                    let bootstrapper =
                        Bootstrapper::<DomainBlock, _, _>::new(consensus_client.clone());
                    match future::select(
                        Box::pin(bootstrapper.fetch_domain_bootstrap_info(self.domain_id)),
                        bootstrapping_worker_drop_receiver,
                    )
                    .await
                    {
                        Left((result, _)) => {
                            let result = result.map_err(|bootstrapping_error| {
                                anyhow!(
                                    "Error while bootstrapping the domain:{} : {:?}",
                                    printable_domain_id,
                                    bootstrapping_error
                                )
                            });
                            let _ = bootstrapping_result_sender.send(result);
                        }
                        Right(_) => {
                            tracing::info!(
                                "Received drop signal while bootstrapping the domain with \
                                 domain_id: {:?}. exiting...",
                                printable_domain_id
                            );
                            let _ = bootstrapping_result_sender.send(Err(anyhow!(
                                "received cancellation signal while bootstrapping the domain: {}.",
                                printable_domain_id
                            )));
                        }
                    };
                }
            },
        );

        destructor_set.add_async_destructor({
            async move {
                let _ = bootstrapping_worker_drop_sender.send(());
                domain_bootstrapper_join_handle.await.expect(
                    "If joining is failing; that means the future being joined panicked, so we \
                     need to propagate it; qed.",
                );
            }
        })?;

        let (domain_runner_result_sender, domain_runner_result_receiver) = oneshot::channel();
        let (domain_runner_drop_sender, mut domain_runner_drop_receiver) = oneshot::channel();
        let domain_runner_join_handle =
            sdk_utils::task_spawn(format!("domain/domain-{}/running", printable_domain_id), {
                let shared_rpc_handler = shared_rpc_handler.clone();
                let shared_progress_data = shared_progress_data.clone();
                async move {
                    let bootstrap_result = match future::select(
                        bootstrapping_result_receiver,
                        &mut domain_runner_drop_receiver,
                    )
                    .await
                    {
                        Left((wrapped_result, _)) => match wrapped_result {
                            Ok(result) => match result {
                                Ok(boostrap_result) => boostrap_result,
                                Err(bootstrap_error) => {
                                    let _ = domain_runner_result_sender.send(Err(anyhow!(
                                        "received an error from domain bootstrapper for domain \
                                         id: {}  error: {}",
                                        printable_domain_id,
                                        bootstrap_error
                                    )));
                                    return;
                                }
                            },
                            Err(recv_err) => {
                                let _ = domain_runner_result_sender.send(Err(anyhow!(
                                    "unable to receive message from domain bootstrapper for \
                                     domain id: {} due to an error: {}",
                                    printable_domain_id,
                                    recv_err
                                )));
                                return;
                            }
                        },
                        Right(_) => {
                            tracing::info!(
                                "Received drop signal while bootstrapping the domain with \
                                 domain_id: {:?}. exiting...",
                                self.domain_id
                            );
                            let _ = domain_runner_result_sender.send(Err(anyhow!(
                                "received cancellation signal while waiting for bootstrapping \
                                 result for domain: {}.",
                                printable_domain_id
                            )));
                            return;
                        }
                    };

                    *shared_progress_data.write().await = DomainBuildingProgress::Bootstrapped;

                    let BootstrapResult {
                        domain_instance_data,
                        domain_created_at,
                        imported_block_notification_stream,
                    } = bootstrap_result;

                    let runtime_type = domain_instance_data.runtime_type.clone();

                    let domain_spec_result = evm_chain_spec::create_domain_spec(
                        self.domain_id,
                        self.chain_id.as_str(),
                        domain_instance_data,
                    );

                    let domain_spec = match domain_spec_result {
                        Ok(domain_spec) => domain_spec,
                        Err(domain_spec_creation_error) => {
                            let _ = domain_runner_result_sender.send(Err(anyhow!(
                                "Error while creating domain spec for the domain: {} Error: {:?}",
                                printable_domain_id,
                                domain_spec_creation_error
                            )));
                            return;
                        }
                    };

                    let domains_directory =
                        directory.as_ref().join(format!("domain-{}", printable_domain_id));
                    let service_config =
                        self.base.configuration(domains_directory, domain_spec).await;

                    let domain_starter = DomainInstanceStarter {
                        service_config,
                        domain_id: self.domain_id,
                        relayer_id: self.relayer_id.clone(),
                        runtime_type,
                        additional_arguments: self.additional_args.clone(),
                        consensus_client,
                        block_importing_notification_stream,
                        new_slot_notification_stream,
                        consensus_network_service,
                        consensus_sync_service,
                        select_chain,
                    };

                    *shared_progress_data.write().await = DomainBuildingProgress::PreparingToStart;

                    let maybe_start_data = domain_starter
                        .prepare_for_start(domain_created_at, imported_block_notification_stream)
                        .await;
                    let (rpc_handler, domain_start_handle) = match maybe_start_data {
                        Ok(start_data) => start_data,
                        Err(start_error) => {
                            let _ = domain_runner_result_sender.send(Err(anyhow!(
                                "Error while preparing to start domain for the domain id: {} \
                                 Error: {:?}",
                                printable_domain_id,
                                start_error
                            )));
                            return;
                        }
                    };

                    let shared_rpc_handler = shared_rpc_handler.clone();
                    shared_rpc_handler.write().await.replace(rpc_handler);

                    *shared_progress_data.write().await = DomainBuildingProgress::Starting;

                    match future::select(domain_start_handle, &mut domain_runner_drop_receiver)
                        .await
                    {
                        Left((wrapped_result, _)) => match wrapped_result {
                            Ok(result) => match result {
                                Ok(_) => {
                                    let _ = domain_runner_result_sender.send(Ok(()));
                                }
                                Err(run_error) => {
                                    let _ = domain_runner_result_sender.send(Err(anyhow!(
                                        "received an error while trying to run the domain id: {}  \
                                         error: {}",
                                        printable_domain_id,
                                        run_error
                                    )));
                                }
                            },
                            Err(join_error) => {
                                let _ = domain_runner_result_sender.send(Err(anyhow!(
                                    "unable to join domain runner for domain id: {} due to an \
                                     error: {}",
                                    printable_domain_id,
                                    join_error
                                )));
                            }
                        },
                        Right(_) => {
                            tracing::info!(
                                "Received drop signal while running the domain with domain_id: \
                                 {:?}. exiting...",
                                self.domain_id
                            );
                            let _ = domain_runner_result_sender.send(Err(anyhow!(
                                "received cancellation signal while waiting for domain runner for \
                                 domain: {}.",
                                printable_domain_id
                            )));
                        }
                    };
                }
            });

        destructor_set.add_async_destructor({
            async move {
                let _ = domain_runner_drop_sender.send(());
                domain_runner_join_handle.await.expect(
                    "If joining is failing; that means the future being joined panicked, so we \
                     need to propagate it; qed.",
                );
            }
        })?;

        Ok(Domain {
            _destructors: destructor_set,
            rpc_handlers: shared_rpc_handler,
            domain_runner_result_receiver,
            current_building_progress: shared_progress_data,
        })
    }
}

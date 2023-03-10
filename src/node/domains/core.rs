//! Core payments domain module

use std::path::Path;
use std::sync::{Arc, Weak};

use anyhow::Context;
use core_payments_domain_runtime::RelayerId;
use derivative::Derivative;
use derive_builder::Builder;
use domain_service::DomainConfiguration;
use futures::prelude::*;
use serde::{Deserialize, Serialize};
use sp_domains::DomainId;

use crate::node::{Base, BaseBuilder, BlockNotification};

pub(crate) mod chain_spec {
    //! Core payments domain chain specs

    use core_payments_domain_runtime::{
        AccountId, BalancesConfig, GenesisConfig, MessengerConfig, SudoConfig, SystemConfig,
        WASM_BINARY,
    };
    use domain_runtime_primitives::RelayerId;
    use sc_service::ChainType;
    use sc_subspace_chain_specs::ExecutionChainSpec;
    use sp_core::crypto::Ss58Codec;
    use subspace_runtime_primitives::SSC;

    use crate::utils::chain_spec::{chain_spec_properties, get_account_id_from_seed};

    /// Chain spec
    pub type ChainSpec = ExecutionChainSpec<GenesisConfig>;

    /// Development config
    pub fn development_config() -> ExecutionChainSpec<GenesisConfig> {
        ExecutionChainSpec::from_genesis(
            // Name
            "Development",
            // ID
            "core_payments_domain_dev",
            ChainType::Development,
            move || {
                testnet_genesis(
                    vec![
                        get_account_id_from_seed("Alice"),
                        get_account_id_from_seed("Bob"),
                        get_account_id_from_seed("Alice//stash"),
                        get_account_id_from_seed("Bob//stash"),
                    ],
                    Some(get_account_id_from_seed("Alice")),
                    vec![(get_account_id_from_seed("Alice"), get_account_id_from_seed("Alice"))],
                )
            },
            vec![],
            None,
            None,
            None,
            Some(chain_spec_properties()),
            None,
        )
    }

    /// Local test net
    pub fn local_testnet_config() -> ExecutionChainSpec<GenesisConfig> {
        ExecutionChainSpec::from_genesis(
            // Name
            "Local Testnet",
            // ID
            "core_payments_domain_local_testnet",
            ChainType::Local,
            move || {
                testnet_genesis(
                    vec![
                        get_account_id_from_seed("Alice"),
                        get_account_id_from_seed("Bob"),
                        get_account_id_from_seed("Charlie"),
                        get_account_id_from_seed("Dave"),
                        get_account_id_from_seed("Eve"),
                        get_account_id_from_seed("Ferdie"),
                        get_account_id_from_seed("Alice//stash"),
                        get_account_id_from_seed("Bob//stash"),
                        get_account_id_from_seed("Charlie//stash"),
                        get_account_id_from_seed("Dave//stash"),
                        get_account_id_from_seed("Eve//stash"),
                        get_account_id_from_seed("Ferdie//stash"),
                    ],
                    Some(get_account_id_from_seed("Alice")),
                    vec![
                        (get_account_id_from_seed("Alice"), get_account_id_from_seed("Alice")),
                        (get_account_id_from_seed("Bob"), get_account_id_from_seed("Bob")),
                    ],
                )
            },
            // Bootnodes
            vec![],
            // Telemetry
            None,
            // Protocol ID
            Some("template-local"),
            None,
            // Properties
            Some(chain_spec_properties()),
            // Extensions
            None,
        )
    }

    /// Gemini 3c chain spec
    pub fn gemini_3c_config() -> ExecutionChainSpec<GenesisConfig> {
        ExecutionChainSpec::from_genesis(
            // Name
            "Subspace Gemini 3c Core Payments Domain",
            // ID
            "subspace_gemini_3c_core_payments_domain",
            ChainType::Local,
            move || {
                testnet_genesis(
                    vec![
                        // Genesis executor
                        AccountId::from_ss58check(
                            "5Df6w8CgYY8kTRwCu8bjBsFu46fy4nFa61xk6dUbL6G4fFjQ",
                        )
                        .expect("Wrong executor account address"),
                    ],
                    None,
                    Default::default(),
                )
            },
            // Bootnodes
            vec![],
            // Telemetry
            None,
            // Protocol ID
            Some("subspace-gemini-3c-core-payments-domain"),
            None,
            // Properties
            Some(chain_spec_properties()),
            // Extensions
            None,
        )
    }

    pub fn devnet_config() -> ChainSpec {
        ChainSpec::from_genesis(
            // Name
            "Subspace Devnet Core Payments Domain",
            // ID
            "subspace_devnet_core_payments_domain",
            ChainType::Custom("Testnet".to_string()),
            move || {
                let sudo_account =
                    AccountId::from_ss58check("5CXTmJEusve5ixyJufqHThmy4qUrrm6FyLCR7QfE4bbyMTNC")
                        .expect("Invalid Sudo account");
                testnet_genesis(
                    vec![
                        // Genesis executor
                        AccountId::from_ss58check(
                            "5Df6w8CgYY8kTRwCu8bjBsFu46fy4nFa61xk6dUbL6G4fFjQ",
                        )
                        .expect("Wrong executor account address"),
                        // Sudo account
                        sudo_account.clone(),
                    ],
                    Some(sudo_account.clone()),
                    vec![(
                        sudo_account,
                        RelayerId::from_ss58check(
                            "5D7kgfacBsP6pkMB628221HG98mz2euaytthdoeZPGceQusS",
                        )
                        .expect("Wrong relayer account address"),
                    )],
                )
            },
            // Bootnodes
            vec![],
            // Telemetry
            None,
            // Protocol ID
            Some("subspace-devnet-core-payments-domain"),
            None,
            // Properties
            Some(chain_spec_properties()),
            // Extensions
            None,
        )
    }

    fn testnet_genesis(
        endowed_accounts: Vec<AccountId>,
        maybe_sudo_account: Option<AccountId>,
        relayers: Vec<(AccountId, RelayerId)>,
    ) -> GenesisConfig {
        GenesisConfig {
            system: SystemConfig {
                code: WASM_BINARY.expect("WASM binary was not build, please build it!").to_vec(),
            },
            sudo: SudoConfig { key: maybe_sudo_account },
            transaction_payment: Default::default(),
            balances: BalancesConfig {
                balances: endowed_accounts.iter().cloned().map(|k| (k, 1_000_000 * SSC)).collect(),
            },
            messenger: MessengerConfig { relayers },
        }
    }
}

/// Core payments domain instance.
pub(crate) struct ExecutorDispatch;

impl sc_executor::NativeExecutionDispatch for ExecutorDispatch {
    // #[cfg(feature = "runtime-benchmarks")]
    // type ExtendHostFunctions = frame_benchmarking::benchmarking::HostFunctions;
    // #[cfg(not(feature = "runtime-benchmarks"))]
    type ExtendHostFunctions = ();

    fn dispatch(method: &str, data: &[u8]) -> Option<Vec<u8>> {
        core_payments_domain_runtime::api::dispatch(method, data)
    }

    fn native_version() -> sc_executor::NativeVersion {
        core_payments_domain_runtime::native_version()
    }
}

/// Node builder
#[derive(Clone, Derivative, Builder, Deserialize, Serialize)]
#[derivative(Debug, PartialEq)]
#[builder(pattern = "immutable", build_fn(private, name = "_build"), name = "ConfigBuilder")]
#[non_exhaustive]
pub struct Config {
    /// Id of the relayer
    #[builder(setter(strip_option), default)]
    #[serde(default, skip_serializing_if = "crate::utils::is_default")]
    pub relayer_id: Option<RelayerId>,
    #[doc(hidden)]
    #[builder(
        setter(into, strip_option),
        field(type = "BaseBuilder", build = "self.base.build()")
    )]
    #[serde(flatten, skip_serializing_if = "crate::utils::is_default")]
    pub base: Base,
}

crate::derive_base!(crate::node::Base => ConfigBuilder);

impl ConfigBuilder {
    /// Constructor
    pub fn new() -> Self {
        Self::default()
    }

    /// Build Config
    pub fn build(&self) -> Config {
        self._build().expect("Infallible")
    }
}

pub(crate) type FullClient =
    domain_service::FullClient<core_payments_domain_runtime::RuntimeApi, ExecutorDispatch>;
pub(crate) type NewFull = domain_service::NewFullCore<
    Arc<FullClient>,
    sc_executor::NativeElseWasmExecutor<ExecutorDispatch>,
    sp_runtime::generic::Block<
        sp_runtime::generic::Header<u32, sp_runtime::traits::BlakeTwo256>,
        sp_runtime::OpaqueExtrinsic,
    >,
    subspace_runtime_primitives::opaque::Block,
    super::FullClient,
    crate::node::FullClient,
    core_payments_domain_runtime::RuntimeApi,
    ExecutorDispatch,
>;
/// Chain spec of the core domain
pub type ChainSpec = chain_spec::ChainSpec;

/// Core domain node
#[derive(Clone, Derivative)]
#[derivative(Debug)]
pub struct CoreDomainNode {
    #[derivative(Debug = "ignore")]
    _client: Weak<FullClient>,
    rpc_handlers: crate::utils::Rpc,
}

impl CoreDomainNode {
    pub(crate) async fn new(
        cfg: Config,
        directory: impl AsRef<Path>,
        chain_spec: ChainSpec,
        primary_chain_node: &mut crate::node::NewFull,
        system_domain_node: &super::NewFull,
        gossip_message_sink: domain_client_message_relayer::GossipMessageSink,
        domain_tx_pool_sinks: &mut impl Extend<(
            DomainId,
            cross_domain_message_gossip::DomainTxPoolSink,
        )>,
    ) -> anyhow::Result<Self> {
        let Config { base, relayer_id: maybe_relayer_id } = cfg;
        let service_config = base.configuration(directory, chain_spec).await;
        let core_domain_config = DomainConfiguration { service_config, maybe_relayer_id };

        // TODO: proper value
        let block_import_throttling_buffer_size = 10;
        let imported_block_notification_stream = primary_chain_node
            .imported_block_notification_stream
            .subscribe()
            .then(|imported_block_notification| async move {
                (
                    imported_block_notification.block_number,
                    imported_block_notification.fork_choice,
                    imported_block_notification.block_import_acknowledgement_sender,
                )
            });
        let new_slot_notification_stream = primary_chain_node
            .new_slot_notification_stream
            .subscribe()
            .then(|slot_notification| async move {
                (
                    slot_notification.new_slot_info.slot,
                    slot_notification.new_slot_info.global_challenge,
                )
            });

        let core_domain_params = domain_service::CoreDomainParams {
            domain_id: DomainId::CORE_PAYMENTS,
            core_domain_config,
            system_domain_client: system_domain_node.client.clone(),
            system_domain_network: system_domain_node.network.clone(),
            primary_chain_client: primary_chain_node.client.clone(),
            primary_network: primary_chain_node.network.clone(),
            select_chain: primary_chain_node.select_chain.clone(),
            imported_block_notification_stream,
            new_slot_notification_stream,
            block_import_throttling_buffer_size,
            gossip_message_sink,
        };

        let NewFull { client, rpc_handlers, tx_pool_sink, task_manager, network_starter, .. } =
            domain_service::new_full_core(core_domain_params).await?;

        domain_tx_pool_sinks.extend([(DomainId::CORE_PAYMENTS, tx_pool_sink)]);
        primary_chain_node.task_manager.add_child(task_manager);

        network_starter.start_network();

        Ok(Self {
            _client: Arc::downgrade(&client),
            rpc_handlers: crate::utils::Rpc::new(&rpc_handlers),
        })
    }

    pub(crate) fn _client(&self) -> anyhow::Result<Arc<FullClient>> {
        self._client.upgrade().ok_or_else(|| anyhow::anyhow!("The node was already closed"))
    }

    /// Subscribe to new blocks imported
    pub async fn subscribe_new_blocks(
        &self,
    ) -> anyhow::Result<impl Stream<Item = BlockNotification> + Send + Sync + Unpin + 'static> {
        self.rpc_handlers.subscribe_new_blocks().await.context("Failed to subscribe to new blocks")
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use tempfile::TempDir;

    use super::*;
    use crate::farmer::CacheDescription;
    use crate::node::{chain_spec, domains, Role};
    use crate::{Farmer, Node, PlotDescription};

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_core() {
        crate::utils::test_init();

        let dir = TempDir::new().unwrap();
        let core = ConfigBuilder::new().build();
        let node = Node::dev()
            .role(Role::Authority)
            .system_domain(domains::ConfigBuilder::new().core(core))
            .build(dir.path(), chain_spec::dev_config().unwrap())
            .await
            .unwrap();
        let (plot_dir, cache_dir) = (TempDir::new().unwrap(), TempDir::new().unwrap());
        let farmer = Farmer::builder()
            .build(
                Default::default(),
                node.clone(),
                &[PlotDescription::minimal(plot_dir.as_ref())],
                CacheDescription::minimal(cache_dir.as_ref()),
            )
            .await
            .unwrap();

        node.system_domain()
            .unwrap()
            .core()
            .unwrap()
            .subscribe_new_blocks()
            .await
            .unwrap()
            .next()
            .await
            .unwrap();

        farmer.close().await.unwrap();
        node.close().await;
    }
}

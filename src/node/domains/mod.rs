//! Module for executor and its domains

use std::path::Path;
use std::sync::{Arc, Weak};

use anyhow::Context;
use core_payments_domain_runtime::RelayerId;
use derivative::Derivative;
use derive_builder::Builder;
use domain_service::DomainConfiguration;
use futures::prelude::*;
use sc_service::ChainSpecExtension;
use serde::{Deserialize, Serialize};
use sp_domains::DomainId;
use subspace_runtime::Block;

use self::core::CoreDomainNode;
use crate::node::{Base, BaseBuilder, BlockNotification};

pub mod core;

pub(crate) mod chain_spec {
    //! System domain chain specs

    use domain_runtime_primitives::RelayerId;
    use frame_support::weights::Weight;
    use sc_service::ChainType;
    use sc_subspace_chain_specs::{ChainSpecExtensions, SerializableChainSpec};
    use sp_core::crypto::Ss58Codec;
    use sp_domains::ExecutorPublicKey;
    use sp_runtime::Percent;
    use subspace_core_primitives::crypto::blake2b_256_hash;
    use subspace_runtime_primitives::SSC;
    use system_domain_runtime::{
        AccountId, Balance, BalancesConfig, DomainRegistryConfig, ExecutorRegistryConfig,
        GenesisConfig, Hash, MessengerConfig, SudoConfig, SystemConfig, WASM_BINARY,
    };

    use crate::utils::chain_spec::{
        chain_spec_properties, get_account_id_from_seed, get_public_key_from_seed,
    };

    type DomainConfig = sp_domains::DomainConfig<Hash, Balance, Weight>;

    /// Chain spec type for the system domain
    pub type ChainSpec = SerializableChainSpec<
        GenesisConfig,
        ChainSpecExtensions<core_payments_domain_runtime::GenesisConfig>,
    >;

    /// Development config
    pub fn development_config() -> ChainSpec {
        ChainSpec::from_genesis(
            // Name
            "Development",
            // ID
            "system_domain_dev",
            ChainType::Development,
            move || {
                testnet_genesis(
                    vec![
                        get_account_id_from_seed("Alice"),
                        get_account_id_from_seed("Bob"),
                        get_account_id_from_seed("Alice//stash"),
                        get_account_id_from_seed("Bob//stash"),
                    ],
                    vec![(
                        get_account_id_from_seed("Alice"),
                        1_000 * SSC,
                        get_account_id_from_seed("Alice"),
                        get_public_key_from_seed::<ExecutorPublicKey>("Alice"),
                    )],
                    vec![(
                        get_account_id_from_seed("Alice"),
                        1_000 * SSC,
                        // TODO: proper genesis domain config
                        DomainConfig {
                            wasm_runtime_hash: blake2b_256_hash(
                                system_domain_runtime::CORE_PAYMENTS_WASM_BUNDLE,
                            )
                            .into(),
                            max_bundle_size: 1024 * 1024,
                            bundle_slot_probability: (1, 1),
                            max_bundle_weight: Weight::MAX,
                            min_operator_stake: 100 * SSC,
                        },
                        get_account_id_from_seed("Alice"),
                        Percent::from_percent(10),
                    )],
                    Some(get_account_id_from_seed("Alice")),
                    vec![(get_account_id_from_seed("Alice"), get_account_id_from_seed("Alice"))],
                )
            },
            vec![],
            None,
            None,
            None,
            Some(chain_spec_properties()),
            ChainSpecExtensions {
                execution_chain_spec: super::core::chain_spec::development_config(),
            },
        )
    }

    /// Local config
    pub fn local_testnet_config() -> ChainSpec {
        ChainSpec::from_genesis(
            // Name
            "Local Testnet",
            // ID
            "system_domain_local_testnet",
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
                    vec![(
                        get_account_id_from_seed("Alice"),
                        1_000 * SSC,
                        get_account_id_from_seed("Alice"),
                        get_public_key_from_seed::<ExecutorPublicKey>("Alice"),
                    )],
                    vec![(
                        get_account_id_from_seed("Alice"),
                        1_000 * SSC,
                        // TODO: proper genesis domain config
                        DomainConfig {
                            wasm_runtime_hash: blake2b_256_hash(
                                system_domain_runtime::CORE_PAYMENTS_WASM_BUNDLE,
                            )
                            .into(),
                            max_bundle_size: 1024 * 1024,
                            bundle_slot_probability: (1, 1),
                            max_bundle_weight: Weight::MAX,
                            min_operator_stake: 100 * SSC,
                        },
                        get_account_id_from_seed("Alice"),
                        Percent::from_percent(10),
                    )],
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
            ChainSpecExtensions {
                execution_chain_spec: super::core::chain_spec::local_testnet_config(),
            },
        )
    }

    /// Gemini 3c config
    pub fn gemini_3c_config() -> ChainSpec {
        ChainSpec::from_genesis(
            // Name
            "Subspace Gemini 3c System Domain",
            // ID
            "subspace_gemini_3c_system_domain",
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
                    vec![(
                        AccountId::from_ss58check(
                            "5Df6w8CgYY8kTRwCu8bjBsFu46fy4nFa61xk6dUbL6G4fFjQ",
                        )
                        .expect("Wrong executor account address"),
                        1_000 * SSC,
                        AccountId::from_ss58check(
                            "5FsxcczkSUnpqhcSgugPZsSghxrcKx5UEsRKL5WyPTL6SAxB",
                        )
                        .expect("Wrong executor reward address"),
                        ExecutorPublicKey::from_ss58check(
                            "5FuuXk1TL8DKQMvg7mcqmP8t9FhxUdzTcYC9aFmebiTLmASx",
                        )
                        .expect("Wrong executor public key"),
                    )],
                    vec![(
                        AccountId::from_ss58check(
                            "5Df6w8CgYY8kTRwCu8bjBsFu46fy4nFa61xk6dUbL6G4fFjQ",
                        )
                        .expect("Wrong executor account address"),
                        1_000 * SSC,
                        DomainConfig {
                            wasm_runtime_hash: blake2b_256_hash(
                                system_domain_runtime::CORE_PAYMENTS_WASM_BUNDLE,
                            )
                            .into(),
                            max_bundle_size: 4 * 1024 * 1024,
                            bundle_slot_probability: (1, 1),
                            max_bundle_weight: Weight::MAX,
                            min_operator_stake: 100 * SSC,
                        },
                        AccountId::from_ss58check(
                            "5Df6w8CgYY8kTRwCu8bjBsFu46fy4nFa61xk6dUbL6G4fFjQ",
                        )
                        .expect("Wrong executor account address"),
                        Percent::from_percent(10),
                    )],
                    None,
                    Default::default(),
                )
            },
            // Bootnodes
            vec![],
            // Telemetry
            None,
            // Protocol ID
            Some("subspace-gemini-3c-system-domain"),
            None,
            // Properties
            Some(chain_spec_properties()),
            // Extensions
            ChainSpecExtensions {
                execution_chain_spec: super::core::chain_spec::gemini_3c_config(),
            },
        )
    }

    pub fn devnet_config() -> ChainSpec {
        ChainSpec::from_genesis(
            // Name
            "Subspace Devnet System domain",
            // ID
            "subspace_devnet_system_domain",
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
                    vec![(
                        AccountId::from_ss58check(
                            "5Df6w8CgYY8kTRwCu8bjBsFu46fy4nFa61xk6dUbL6G4fFjQ",
                        )
                        .expect("Wrong executor account address"),
                        1_000 * SSC,
                        AccountId::from_ss58check(
                            "5FsxcczkSUnpqhcSgugPZsSghxrcKx5UEsRKL5WyPTL6SAxB",
                        )
                        .expect("Wrong executor reward address"),
                        ExecutorPublicKey::from_ss58check(
                            "5FuuXk1TL8DKQMvg7mcqmP8t9FhxUdzTcYC9aFmebiTLmASx",
                        )
                        .expect("Wrong executor public key"),
                    )],
                    vec![(
                        AccountId::from_ss58check(
                            "5Df6w8CgYY8kTRwCu8bjBsFu46fy4nFa61xk6dUbL6G4fFjQ",
                        )
                        .expect("Wrong executor account address"),
                        1_000 * SSC,
                        DomainConfig {
                            wasm_runtime_hash: blake2b_256_hash(
                                system_domain_runtime::CORE_PAYMENTS_WASM_BUNDLE,
                            )
                            .into(),
                            max_bundle_size: 4 * 1024 * 1024,
                            bundle_slot_probability: (1, 1),
                            max_bundle_weight: Weight::MAX,
                            min_operator_stake: 100 * SSC,
                        },
                        AccountId::from_ss58check(
                            "5Df6w8CgYY8kTRwCu8bjBsFu46fy4nFa61xk6dUbL6G4fFjQ",
                        )
                        .expect("Wrong executor account address"),
                        Percent::from_percent(10),
                    )],
                    Some(sudo_account.clone()),
                    vec![(
                        sudo_account,
                        RelayerId::from_ss58check(
                            "5D7kgfacBsP6pkMB628221HG98mz2euaytthdoeZPGceQusS",
                        )
                        .expect("Invalid relayer id account"),
                    )],
                )
            },
            // Bootnodes
            vec![],
            // Telemetry
            None,
            // Protocol ID
            Some("subspace-devnet-execution"),
            None,
            // Properties
            Some(chain_spec_properties()),
            // Extensions
            ChainSpecExtensions { execution_chain_spec: super::core::chain_spec::devnet_config() },
        )
    }

    fn testnet_genesis(
        endowed_accounts: Vec<AccountId>,
        executors: Vec<(AccountId, Balance, AccountId, ExecutorPublicKey)>,
        domains: Vec<(AccountId, Balance, DomainConfig, AccountId, Percent)>,
        maybe_sudo_account: Option<AccountId>,
        relayers: Vec<(AccountId, RelayerId)>,
    ) -> GenesisConfig {
        GenesisConfig {
            system: SystemConfig {
                code: WASM_BINARY.expect("WASM binary was not build, please build it!").to_vec(),
            },
            sudo: SudoConfig {
                // Assign network admin rights.
                key: maybe_sudo_account,
            },
            transaction_payment: Default::default(),
            balances: BalancesConfig {
                balances: endowed_accounts.iter().cloned().map(|k| (k, 1_000_000 * SSC)).collect(),
            },
            executor_registry: ExecutorRegistryConfig { executors, slot_probability: (1, 1) },
            domain_registry: DomainRegistryConfig { domains },
            messenger: MessengerConfig { relayers },
        }
    }
}

/// System domain executor instance.
pub(crate) struct ExecutorDispatch;

impl sc_executor::NativeExecutionDispatch for ExecutorDispatch {
    // #[cfg(feature = "runtime-benchmarks")]
    // type ExtendHostFunctions = frame_benchmarking::benchmarking::HostFunctions;
    // #[cfg(not(feature = "runtime-benchmarks"))]
    type ExtendHostFunctions = ();

    fn dispatch(method: &str, data: &[u8]) -> Option<Vec<u8>> {
        system_domain_runtime::api::dispatch(method, data)
    }

    fn native_version() -> sc_executor::NativeVersion {
        system_domain_runtime::native_version()
    }
}

/// Node builder
#[derive(Debug, Clone, Derivative, Builder, Deserialize, Serialize, PartialEq)]
#[derivative(Default)]
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
    #[serde(default, skip_serializing_if = "crate::utils::is_default")]
    pub base: Base,
    /// The core config
    #[builder(setter(strip_option), default)]
    #[serde(default, skip_serializing_if = "crate::utils::is_default")]
    pub core: Option<core::Config>,
}

crate::derive_base!(crate::node::Base => ConfigBuilder);

pub(crate) type FullClient =
    domain_service::FullClient<system_domain_runtime::RuntimeApi, ExecutorDispatch>;
pub(crate) type NewFull = domain_service::NewFullSystem<
    Arc<FullClient>,
    sc_executor::NativeElseWasmExecutor<ExecutorDispatch>,
    subspace_runtime_primitives::opaque::Block,
    crate::node::FullClient,
    system_domain_runtime::RuntimeApi,
    ExecutorDispatch,
>;
/// Chain spec of the system domain
pub type ChainSpec = chain_spec::ChainSpec;

/// System domain node
#[derive(Clone, Derivative)]
#[derivative(Debug)]
pub struct SystemDomainNode {
    #[derivative(Debug = "ignore")]
    _client: Weak<FullClient>,
    core: Option<CoreDomainNode>,
    rpc_handlers: crate::utils::Rpc,
}

impl SystemDomainNode {
    pub(crate) async fn new(
        cfg: Config,
        directory: impl AsRef<Path>,
        chain_spec: ChainSpec,
        primary_new_full: &mut crate::node::NewFull,
    ) -> anyhow::Result<Self> {
        let Config { base, relayer_id: maybe_relayer_id, core } = cfg;
        let maybe_core_domain_spec = chain_spec
            .extensions()
            .get_any(std::any::TypeId::of::<core::ChainSpec>())
            .downcast_ref()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Core domain is not supported"));
        let service_config =
            base.configuration(directory.as_ref().join("system"), chain_spec).await;

        let system_domain_config = DomainConfiguration { service_config, maybe_relayer_id };
        let imported_block_notification_stream = primary_new_full
            .imported_block_notification_stream
            .subscribe()
            .then(|imported_block_notification| async move {
                (
                    imported_block_notification.block_number,
                    imported_block_notification.block_import_acknowledgement_sender,
                )
            });

        let new_slot_notification_stream = primary_new_full
            .new_slot_notification_stream
            .subscribe()
            .then(|slot_notification| async move {
                (
                    slot_notification.new_slot_info.slot,
                    slot_notification.new_slot_info.global_challenge,
                )
            });

        let (gossip_msg_sink, gossip_msg_stream) =
            sc_utils::mpsc::tracing_unbounded("Cross domain gossip messages", 100);

        // TODO: proper value
        let block_import_throttling_buffer_size = 10;

        let system_domain_node = domain_service::new_full_system(
            system_domain_config,
            primary_new_full.client.clone(),
            primary_new_full.backend.clone(),
            primary_new_full.network.clone(),
            &primary_new_full.select_chain,
            imported_block_notification_stream,
            new_slot_notification_stream,
            block_import_throttling_buffer_size,
            gossip_msg_sink.clone(),
        )
        .await?;

        let mut domain_tx_pool_sinks = std::collections::BTreeMap::new();

        let core = if let Some(core) = core {
            let core_domain_id = u32::from(DomainId::CORE_PAYMENTS);
            CoreDomainNode::new(
                core,
                directory.as_ref().join(format!("core-{core_domain_id}")),
                maybe_core_domain_spec?,
                primary_new_full,
                &system_domain_node,
                gossip_msg_sink.clone(),
                &mut domain_tx_pool_sinks,
            )
            .await
            .map(Some)?
        } else {
            None
        };

        domain_tx_pool_sinks.insert(DomainId::SYSTEM, system_domain_node.tx_pool_sink);
        primary_new_full.task_manager.add_child(system_domain_node.task_manager);

        let cross_domain_message_gossip_worker =
            cross_domain_message_gossip::GossipWorker::<Block>::new(
                primary_new_full.network.clone(),
                domain_tx_pool_sinks,
            );

        let NewFull { client, network_starter, rpc_handlers, .. } = system_domain_node;

        tokio::spawn(cross_domain_message_gossip_worker.run(gossip_msg_stream));
        network_starter.start_network();

        Ok(Self {
            _client: Arc::downgrade(&client),
            core,
            rpc_handlers: crate::utils::Rpc::new(&rpc_handlers),
        })
    }

    pub(crate) fn _client(&self) -> anyhow::Result<Arc<FullClient>> {
        self._client.upgrade().ok_or_else(|| anyhow::anyhow!("The node was already closed"))
    }

    /// Get the core node handler
    pub fn core(&self) -> Option<CoreDomainNode> {
        self.core.clone()
    }

    /// Subscribe to new blocks imported
    pub async fn subscribe_new_blocks(
        &self,
    ) -> anyhow::Result<impl Stream<Item = BlockNotification> + Send + Sync + Unpin + 'static> {
        self.rpc_handlers.subscribe_new_blocks().await.context("Failed to subscribe to new blocks")
    }
}

crate::generate_builder!(Config);

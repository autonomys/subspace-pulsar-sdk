//! System domain chain specs

use frame_support::weights::Weight;
use sc_service::ChainType;
use sc_subspace_chain_specs::SerializableChainSpec;
use sdk_utils::chain_spec::{
    chain_spec_properties, get_account_id_from_seed, get_public_key_from_seed,
};
use sp_core::crypto::Ss58Codec;
use sp_domains::ExecutorPublicKey;
use sp_runtime::Percent;
use subspace_core_primitives::crypto::blake2b_256_hash;
use subspace_runtime_primitives::SSC;
use system_domain_runtime::{
    AccountId, Balance, BalancesConfig, DomainRegistryConfig, ExecutorRegistryConfig,
    GenesisConfig, Hash, MessengerConfig, SudoConfig, SystemConfig, WASM_BINARY,
};

type DomainConfig = sp_domains::DomainConfig<Hash, Balance, Weight>;

/// The extensions for the [`ConsensusChainSpec`].
#[derive(Clone, serde::Serialize, serde::Deserialize, sc_chain_spec::ChainSpecExtension)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ChainSpecExtensions {
    #[cfg(feature = "core-payments")]
    pub core: Option<SerializableChainSpec<core_payments_domain_runtime::GenesisConfig>>,
    #[cfg(feature = "eth-relayer")]
    pub eth: Option<SerializableChainSpec<core_eth_relay_runtime::GenesisConfig>>,
    #[cfg(feature = "core-evm")]
    pub evm: Option<SerializableChainSpec<core_evm_runtime::GenesisConfig>>,
    #[serde(skip)]
    pub _if_no_fields_in_extension_macro_panics: (),
}

/// Chain spec type for the system domain
pub type ChainSpec = SerializableChainSpec<GenesisConfig, ChainSpecExtensions>;

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
            #[cfg(feature = "core-payments")]
            core: Some(super::core_payments::chain_spec::development_config()),
            #[cfg(feature = "eth-relayer")]
            eth: Some(super::eth_relayer::chain_spec::development_config()),
            #[cfg(feature = "core-evm")]
            evm: Some(super::evm::chain_spec::development_config()),
            _if_no_fields_in_extension_macro_panics: (),
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
            #[cfg(feature = "core-payments")]
            core: Some(super::core_payments::chain_spec::local_testnet_config()),
            #[cfg(feature = "eth-relayer")]
            eth: Some(super::eth_relayer::chain_spec::local_testnet_config()),
            #[cfg(feature = "core-evm")]
            evm: Some(super::evm::chain_spec::local_testnet_config()),
            _if_no_fields_in_extension_macro_panics: (),
        },
    )
}

/// Gemini 3d config
pub fn gemini_3d_config() -> ChainSpec {
    ChainSpec::from_genesis(
        // Name
        "Subspace Gemini 3d System Domain",
        // ID
        "subspace_gemini_3d_system_domain",
        ChainType::Live,
        move || {
            let sudo_account =
                AccountId::from_ss58check("5CZy4hcmaVZUMZLfB41v1eAKvtZ8W7axeWuDvwjhjPwfhAqt")
                    .expect("Invalid Sudo account.");
            testnet_genesis(
                vec![
                    // Genesis executor
                    AccountId::from_ss58check("5Df6w8CgYY8kTRwCu8bjBsFu46fy4nFa61xk6dUbL6G4fFjQ")
                        .expect("Wrong executor account address"),
                    // Sudo account
                    sudo_account.clone(),
                ],
                vec![(
                    AccountId::from_ss58check("5Df6w8CgYY8kTRwCu8bjBsFu46fy4nFa61xk6dUbL6G4fFjQ")
                        .expect("Wrong executor account address"),
                    1_000 * SSC,
                    AccountId::from_ss58check("5FsxcczkSUnpqhcSgugPZsSghxrcKx5UEsRKL5WyPTL6SAxB")
                        .expect("Wrong executor reward address"),
                    ExecutorPublicKey::from_ss58check(
                        "5FuuXk1TL8DKQMvg7mcqmP8t9FhxUdzTcYC9aFmebiTLmASx",
                    )
                    .expect("Wrong executor public key"),
                )],
                vec![(
                    AccountId::from_ss58check("5Df6w8CgYY8kTRwCu8bjBsFu46fy4nFa61xk6dUbL6G4fFjQ")
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
                    AccountId::from_ss58check("5Df6w8CgYY8kTRwCu8bjBsFu46fy4nFa61xk6dUbL6G4fFjQ")
                        .expect("Wrong executor account address"),
                    Percent::from_percent(10),
                )],
                Some(sudo_account),
                Default::default(),
            )
        },
        // Bootnodes
        vec![],
        // Telemetry
        None,
        // Protocol ID
        Some("subspace-gemini-3d-system-domain"),
        None,
        // Properties
        Some(chain_spec_properties()),
        // Extensions
        ChainSpecExtensions {
            #[cfg(feature = "core-payments")]
            core: Some(super::core_payments::chain_spec::gemini_3d_config()),
            #[cfg(feature = "eth-relayer")]
            eth: Some(super::eth_relayer::chain_spec::gemini_3d_config()),
            #[cfg(feature = "core-evm")]
            evm: Some(super::evm::chain_spec::gemini_3d_config()),
            _if_no_fields_in_extension_macro_panics: (),
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
                AccountId::from_ss58check("5CZy4hcmaVZUMZLfB41v1eAKvtZ8W7axeWuDvwjhjPwfhAqt")
                    .expect("Invalid Sudo account");
            testnet_genesis(
                vec![
                    // Genesis executor
                    AccountId::from_ss58check("5Df6w8CgYY8kTRwCu8bjBsFu46fy4nFa61xk6dUbL6G4fFjQ")
                        .expect("Wrong executor account address"),
                    // Sudo account
                    sudo_account.clone(),
                ],
                vec![(
                    AccountId::from_ss58check("5Df6w8CgYY8kTRwCu8bjBsFu46fy4nFa61xk6dUbL6G4fFjQ")
                        .expect("Wrong executor account address"),
                    1_000 * SSC,
                    AccountId::from_ss58check("5FsxcczkSUnpqhcSgugPZsSghxrcKx5UEsRKL5WyPTL6SAxB")
                        .expect("Wrong executor reward address"),
                    ExecutorPublicKey::from_ss58check(
                        "5FuuXk1TL8DKQMvg7mcqmP8t9FhxUdzTcYC9aFmebiTLmASx",
                    )
                    .expect("Wrong executor public key"),
                )],
                vec![(
                    AccountId::from_ss58check("5Df6w8CgYY8kTRwCu8bjBsFu46fy4nFa61xk6dUbL6G4fFjQ")
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
                    AccountId::from_ss58check("5Df6w8CgYY8kTRwCu8bjBsFu46fy4nFa61xk6dUbL6G4fFjQ")
                        .expect("Wrong executor account address"),
                    Percent::from_percent(10),
                )],
                Some(sudo_account.clone()),
                vec![(
                    sudo_account,
                    AccountId::from_ss58check("5D7kgfacBsP6pkMB628221HG98mz2euaytthdoeZPGceQusS")
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
        ChainSpecExtensions {
            #[cfg(feature = "core-payments")]
            core: Some(super::core_payments::chain_spec::devnet_config()),
            #[cfg(feature = "eth-relayer")]
            eth: Some(super::eth_relayer::chain_spec::devnet_config()),
            #[cfg(feature = "core-evm")]
            evm: Some(super::evm::chain_spec::devnet_config()),
            _if_no_fields_in_extension_macro_panics: (),
        },
    )
}

fn testnet_genesis(
    endowed_accounts: Vec<AccountId>,
    executors: Vec<(AccountId, Balance, AccountId, ExecutorPublicKey)>,
    domains: Vec<(AccountId, Balance, DomainConfig, AccountId, Percent)>,
    maybe_sudo_account: Option<AccountId>,
    relayers: Vec<(AccountId, AccountId)>,
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

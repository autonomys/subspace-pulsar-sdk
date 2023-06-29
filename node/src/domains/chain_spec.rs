//! System domain chain specs

use sc_service::ChainType;
use sc_subspace_chain_specs::SerializableChainSpec;
use sdk_utils::chain_spec::{
    chain_spec_properties, get_account_id_from_seed, get_public_key_from_seed,
};
use sp_core::crypto::Ss58Codec;
use sp_domains::ExecutorPublicKey;
use subspace_runtime_primitives::SSC;
use system_domain_runtime::{
    AccountId, Balance, BalancesConfig, DomainRegistryConfig, ExecutorRegistryConfig,
    GenesisConfig, MessengerConfig, SudoConfig, SystemConfig, WASM_BINARY,
};

/// Chain spec type for the system domain
pub type ChainSpec = SerializableChainSpec<GenesisConfig>;

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

/// Gemini 3e config
pub fn gemini_3e_config() -> ChainSpec {
    ChainSpec::from_genesis(
        // Name
        "Subspace Gemini 3e System Domain",
        // ID
        "subspace_gemini_3e_system_domain",
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
                Some(sudo_account),
                Default::default(),
            )
        },
        // Bootnodes
        vec![],
        // Telemetry
        None,
        // Protocol ID
        Some("subspace-gemini-3e-system-domain"),
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
        None,
    )
}

fn testnet_genesis(
    endowed_accounts: Vec<AccountId>,
    executors: Vec<(AccountId, Balance, AccountId, ExecutorPublicKey)>,
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
        domain_registry: DomainRegistryConfig::default(),
        messenger: MessengerConfig { relayers },
    }
}

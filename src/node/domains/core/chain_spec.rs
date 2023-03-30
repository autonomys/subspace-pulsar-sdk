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

/// Gemini 3d chain spec
pub fn gemini_3d_config() -> ChainSpec {
    ChainSpec::from_genesis(
        // Name
        "Subspace Gemini 3d Core Payments Domain",
        // ID
        "subspace_gemini_3d_core_payments_domain",
        ChainType::Live,
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
                Some(sudo_account),
                Default::default(),
            )
        },
        // Bootnodes
        vec![],
        // Telemetry
        None,
        // Protocol ID
        Some("subspace-gemini-3d-core-payments-domain"),
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
                    AccountId::from_ss58check("5Df6w8CgYY8kTRwCu8bjBsFu46fy4nFa61xk6dUbL6G4fFjQ")
                        .expect("Wrong executor account address"),
                    // Sudo account
                    sudo_account.clone(),
                ],
                Some(sudo_account.clone()),
                vec![(
                    sudo_account,
                    RelayerId::from_ss58check("5D7kgfacBsP6pkMB628221HG98mz2euaytthdoeZPGceQusS")
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

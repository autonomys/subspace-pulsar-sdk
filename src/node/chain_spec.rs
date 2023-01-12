//! Subspace chain configurations.

use sc_service::ChainType;
use sc_subspace_chain_specs::{ChainSpecExtensions, ConsensusChainSpec};
use sc_telemetry::TelemetryEndpoints;
use sp_core::crypto::{Ss58Codec, UncheckedFrom};
use subspace_runtime::{
    AllowAuthoringBy, BalancesConfig, GenesisConfig, RuntimeConfigsConfig, SubspaceConfig,
    SudoConfig, SystemConfig, VestingConfig, MILLISECS_PER_BLOCK, WASM_BINARY,
};
use subspace_runtime_primitives::{AccountId, Balance, BlockNumber, SSC};
use system_domain_runtime::GenesisConfig as SystemDomainGenesisConfig;

const SUBSPACE_TELEMETRY_URL: &str = "wss://telemetry.subspace.network/submit/";
const X_NET_2_CHAIN_SPEC: &[u8] = include_bytes!("../../res/chain-spec-raw-x-net-2.json");
const GEMINI_3B_CHAIN_SPEC: &[u8] = include_bytes!("../../res/chain-spec-raw-gemini-3b.json");

/// List of accounts which should receive token grants, amounts are specified in
/// SSC.
const TOKEN_GRANTS: &[(&str, u128)] = &[
    ("5Dns1SVEeDqnbSm2fVUqHJPCvQFXHVsgiw28uMBwmuaoKFYi", 3_000_000),
    ("5DxtHHQL9JGapWCQARYUAWj4yDcwuhg9Hsk5AjhEzuzonVyE", 1_500_000),
    ("5EHhw9xuQNdwieUkNoucq2YcateoMVJQdN8EZtmRy3roQkVK", 133_333),
    ("5C5qYYCQBnanGNPGwgmv6jiR2MxNPrGnWYLPFEyV1Xdy2P3x", 178_889),
    ("5GBWVfJ253YWVPHzWDTos1nzYZpa9TemP7FpQT9RnxaFN6Sz", 350_000),
    ("5F9tEPid88uAuGbjpyegwkrGdkXXtaQ9sGSWEnYrfVCUCsen", 111_111),
    ("5DkJFCv3cTBsH5y1eFT94DXMxQ3EmVzYojEA88o56mmTKnMp", 244_444),
    ("5G23o1yxWgVNQJuL4Y9UaCftAFvLuMPCRe7BCARxCohjoHc9", 311_111),
    ("5GhHwuJoK1b7uUg5oi8qUXxWHdfgzv6P5CQSdJ3ffrnPRgKM", 317_378),
    ("5EqBwtqrCV427xCtTsxnb9X2Qay39pYmKNk9wD9Kd62jLS97", 300_000),
    ("5D9pNnGCiZ9UqhBQn5n71WFVaRLvZ7znsMvcZ7PHno4zsiYa", 600_000),
    ("5DXfPcXUcP4BG8LBSkJDrfFNApxjWySR6ARfgh3v27hdYr5S", 430_000),
    ("5CXSdDJgzRTj54f9raHN2Z5BNPSMa2ETjqCTUmpaw3ECmwm4", 330_000),
    ("5DqKxL7bQregQmUfFgzTMfRKY4DSvA1KgHuurZWYmxYSCmjY", 200_000),
    ("5CfixiS93yTwHQbzzfn8P2tMxhKXdTx7Jam9htsD7XtiMFtn", 27_800),
    ("5FZe9YzXeEXe7sK5xLR8yCmbU8bPJDTZpNpNbToKvSJBUiEo", 18_067),
    ("5FZwEgsvZz1vpeH7UsskmNmTpbfXvAcojjgVfShgbRqgC1nx", 27_800),
];

/// Additional subspace specific genesis parameters.
pub struct GenesisParams {
    enable_rewards: bool,
    enable_storage_access: bool,
    allow_authoring_by: AllowAuthoringBy,
    enable_executor: bool,
}

/// Gemini 3b chain spec
pub fn gemini_3b() -> Result<ConsensusChainSpec<GenesisConfig, SystemDomainGenesisConfig>, String> {
    ConsensusChainSpec::from_json_bytes(GEMINI_3B_CHAIN_SPEC)
}

/// Gemini 3b compiled chain spec
pub fn gemini_3b_compiled(
) -> Result<ConsensusChainSpec<GenesisConfig, SystemDomainGenesisConfig>, String> {
    Ok(ConsensusChainSpec::from_genesis(
        // Name
        "Subspace Gemini 3b",
        // ID
        "subspace_gemini_3b",
        ChainType::Custom("Subspace Gemini 3b".to_string()),
        || {
            let sudo_account =
                AccountId::from_ss58check("5CXTmJEusve5ixyJufqHThmy4qUrrm6FyLCR7QfE4bbyMTNC")
                    .expect("Wrong root account address");

            let mut balances = vec![(sudo_account.clone(), 1_000 * SSC)];
            let vesting_schedules = TOKEN_GRANTS
                .iter()
                .flat_map(|&(account_address, amount)| {
                    let account_id = AccountId::from_ss58check(account_address)
                        .expect("Wrong vesting account address");
                    let amount: Balance = amount * SSC;

                    // TODO: Adjust start block to real value before mainnet launch
                    let start_block = 100_000_000;
                    let one_month_in_blocks =
                        u32::try_from(3600 * 24 * 30 * MILLISECS_PER_BLOCK / 1000)
                            .expect("One month of blocks always fits in u32; qed");

                    // Add balance so it can be locked
                    balances.push((account_id.clone(), amount));

                    [
                        // 1/4 of tokens are released after 1 year.
                        (account_id.clone(), start_block, one_month_in_blocks * 12, 1, amount / 4),
                        // 1/48 of tokens are released every month after that for 3 more years.
                        (
                            account_id,
                            start_block + one_month_in_blocks * 12,
                            one_month_in_blocks,
                            36,
                            amount / 48,
                        ),
                    ]
                })
                .collect::<Vec<_>>();
            subspace_genesis_config(
                WASM_BINARY.expect("Wasm binary must be built for Gemini"),
                sudo_account,
                balances,
                vesting_schedules,
                GenesisParams {
                    enable_rewards: false,
                    enable_storage_access: false,
                    allow_authoring_by: AllowAuthoringBy::RootFarmer(
                        sp_consensus_subspace::FarmerPublicKey::unchecked_from([
                            0x50, 0x69, 0x60, 0xf3, 0x50, 0x33, 0xee, 0xc1, 0x12, 0xb5, 0xbc, 0xb4,
                            0xe5, 0x91, 0xfb, 0xbb, 0xf5, 0x88, 0xac, 0x45, 0x26, 0x90, 0xd4, 0x70,
                            0x32, 0x6c, 0x3f, 0x7b, 0x4e, 0xd9, 0x41, 0x17,
                        ]),
                    ),
                    enable_executor: true,
                },
            )
        },
        // Bootnodes
        vec![],
        // Telemetry
        Some(
            TelemetryEndpoints::new(vec![(SUBSPACE_TELEMETRY_URL.into(), 1)])
                .map_err(|error| error.to_string())?,
        ),
        // Protocol ID
        Some("subspace-gemini-3b"),
        None,
        // Properties
        Some(utils::chain_spec_properties()),
        // Extensions
        ChainSpecExtensions { execution_chain_spec: system_domain::gemini_3b_config() },
    ))
}

/// Executor net 2 chain spec
pub fn x_net_2_config(
) -> Result<ConsensusChainSpec<GenesisConfig, SystemDomainGenesisConfig>, String> {
    ConsensusChainSpec::from_json_bytes(X_NET_2_CHAIN_SPEC)
}

/// Executor net 2 chain spec
pub fn x_net_2_config_compiled(
) -> Result<ConsensusChainSpec<GenesisConfig, SystemDomainGenesisConfig>, String> {
    Ok(ConsensusChainSpec::from_genesis(
        // Name
        "Subspace X-Net 2",
        // ID
        "subspace_x_net_2a",
        ChainType::Custom("Subspace X-Net 2".to_string()),
        || {
            let sudo_account =
                AccountId::from_ss58check("5CXTmJEusve5ixyJufqHThmy4qUrrm6FyLCR7QfE4bbyMTNC")
                    .expect("Wrong root account address");

            let mut balances = vec![(sudo_account.clone(), 1_000 * SSC)];
            let vesting_schedules = TOKEN_GRANTS
                .iter()
                .flat_map(|&(account_address, amount)| {
                    let account_id = AccountId::from_ss58check(account_address)
                        .expect("Wrong vesting account address");
                    let amount: Balance = amount * SSC;

                    // TODO: Adjust start block to real value before mainnet launch
                    let start_block = 100_000_000;
                    let one_month_in_blocks =
                        u32::try_from(3600 * 24 * 30 * MILLISECS_PER_BLOCK / 1000)
                            .expect("One month of blocks always fits in u32; qed");

                    // Add balance so it can be locked
                    balances.push((account_id.clone(), amount));

                    [
                        // 1/4 of tokens are released after 1 year.
                        (account_id.clone(), start_block, one_month_in_blocks * 12, 1, amount / 4),
                        // 1/48 of tokens are released every month after that for 3 more years.
                        (
                            account_id,
                            start_block + one_month_in_blocks * 12,
                            one_month_in_blocks,
                            36,
                            amount / 48,
                        ),
                    ]
                })
                .collect::<Vec<_>>();
            subspace_genesis_config(
                WASM_BINARY.expect("Wasm binary must be built for Gemini"),
                sudo_account,
                balances,
                vesting_schedules,
                GenesisParams {
                    enable_rewards: false,
                    enable_storage_access: false,
                    allow_authoring_by: AllowAuthoringBy::FirstFarmer,
                    enable_executor: true,
                },
            )
        },
        // Bootnodes
        vec![],
        // Telemetry
        Some(
            TelemetryEndpoints::new(vec![(SUBSPACE_TELEMETRY_URL.into(), 1)])
                .map_err(|error| error.to_string())?,
        ),
        // Protocol ID
        Some("subspace-x-net-2a"),
        None,
        // Properties
        Some(utils::chain_spec_properties()),
        // Extensions
        ChainSpecExtensions { execution_chain_spec: system_domain::x_net_2_config() },
    ))
}

/// New dev chain spec
pub fn dev_config() -> Result<ConsensusChainSpec<GenesisConfig, SystemDomainGenesisConfig>, String>
{
    let wasm_binary = WASM_BINARY.ok_or_else(|| "Development wasm not available".to_string())?;

    Ok(ConsensusChainSpec::from_genesis(
        // Name
        "Subspace development",
        // ID
        "subspace_dev",
        ChainType::Development,
        || {
            subspace_genesis_config(
                wasm_binary,
                // Sudo account
                utils::get_account_id_from_seed("Alice"),
                // Pre-funded accounts
                vec![
                    (utils::get_account_id_from_seed("Alice"), 1_000 * SSC),
                    (utils::get_account_id_from_seed("Bob"), 1_000 * SSC),
                    (utils::get_account_id_from_seed("Alice//stash"), 1_000 * SSC),
                    (utils::get_account_id_from_seed("Bob//stash"), 1_000 * SSC),
                ],
                vec![],
                GenesisParams {
                    enable_rewards: false,
                    enable_storage_access: false,
                    allow_authoring_by: AllowAuthoringBy::Anyone,
                    enable_executor: true,
                },
            )
        },
        // Bootnodes
        vec![],
        // Telemetry
        None,
        // Protocol ID
        None,
        None,
        // Properties
        Some(utils::chain_spec_properties()),
        // Extensions
        ChainSpecExtensions { execution_chain_spec: system_domain::development_config() },
    ))
}

/// New local chain spec
pub fn local_config() -> Result<ConsensusChainSpec<GenesisConfig, SystemDomainGenesisConfig>, String>
{
    let wasm_binary = WASM_BINARY.ok_or_else(|| "Development wasm not available".to_string())?;

    Ok(ConsensusChainSpec::from_genesis(
        // Name
        "Subspace local",
        // ID
        "subspace_local",
        ChainType::Local,
        || {
            subspace_genesis_config(
                wasm_binary,
                // Sudo account
                utils::get_account_id_from_seed("Alice"),
                // Pre-funded accounts
                vec![
                    (utils::get_account_id_from_seed("Alice"), 1_000 * SSC),
                    (utils::get_account_id_from_seed("Bob"), 1_000 * SSC),
                    (utils::get_account_id_from_seed("Charlie"), 1_000 * SSC),
                    (utils::get_account_id_from_seed("Dave"), 1_000 * SSC),
                    (utils::get_account_id_from_seed("Eve"), 1_000 * SSC),
                    (utils::get_account_id_from_seed("Ferdie"), 1_000 * SSC),
                    (utils::get_account_id_from_seed("Alice//stash"), 1_000 * SSC),
                    (utils::get_account_id_from_seed("Bob//stash"), 1_000 * SSC),
                    (utils::get_account_id_from_seed("Charlie//stash"), 1_000 * SSC),
                    (utils::get_account_id_from_seed("Dave//stash"), 1_000 * SSC),
                    (utils::get_account_id_from_seed("Eve//stash"), 1_000 * SSC),
                    (utils::get_account_id_from_seed("Ferdie//stash"), 1_000 * SSC),
                ],
                vec![],
                GenesisParams {
                    enable_rewards: false,
                    enable_storage_access: false,
                    allow_authoring_by: AllowAuthoringBy::Anyone,
                    enable_executor: true,
                },
            )
        },
        // Bootnodes
        vec![],
        // Telemetry
        None,
        // Protocol ID
        None,
        None,
        // Properties
        Some(utils::chain_spec_properties()),
        // Extensions
        ChainSpecExtensions { execution_chain_spec: system_domain::local_testnet_config() },
    ))
}

/// Configure initial storage state for FRAME modules.
pub fn subspace_genesis_config(
    wasm_binary: &[u8],
    sudo_account: AccountId,
    balances: Vec<(AccountId, Balance)>,
    // who, start, period, period_count, per_period
    vesting: Vec<(AccountId, BlockNumber, BlockNumber, u32, Balance)>,
    genesis_params: GenesisParams,
) -> GenesisConfig {
    let GenesisParams {
        enable_rewards,
        enable_storage_access,
        allow_authoring_by,
        enable_executor,
    } = genesis_params;

    GenesisConfig {
        system: SystemConfig {
            // Add Wasm runtime to storage.
            code: wasm_binary.to_vec(),
        },
        balances: BalancesConfig { balances },
        transaction_payment: Default::default(),
        sudo: SudoConfig {
            // Assign network admin rights.
            key: Some(sudo_account),
        },
        subspace: SubspaceConfig { enable_rewards, enable_storage_access, allow_authoring_by },
        vesting: VestingConfig { vesting },
        runtime_configs: RuntimeConfigsConfig { enable_executor },
    }
}

pub mod system_domain {
    //! System domain chain specs

    use domain_runtime_primitives::RelayerId;
    use frame_support::weights::Weight;
    use sc_service::ChainType;
    use sc_subspace_chain_specs::ExecutionChainSpec;
    use sp_core::crypto::Ss58Codec;
    use sp_domains::ExecutorPublicKey;
    use sp_runtime::Percent;
    use subspace_core_primitives::crypto::blake2b_256_hash;
    use subspace_runtime_primitives::SSC;
    use system_domain_runtime::{
        AccountId, Balance, BalancesConfig, DomainRegistryConfig, ExecutorRegistryConfig,
        GenesisConfig, Hash, MessengerConfig, SudoConfig, SystemConfig, WASM_BINARY,
    };

    use super::utils::{chain_spec_properties, get_account_id_from_seed, get_public_key_from_seed};

    type DomainConfig = sp_domains::DomainConfig<Hash, Balance, Weight>;

    /// Development config
    pub fn development_config() -> ExecutionChainSpec<GenesisConfig> {
        ExecutionChainSpec::from_genesis(
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
                        Percent::one(),
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
    pub fn local_testnet_config() -> ExecutionChainSpec<GenesisConfig> {
        ExecutionChainSpec::from_genesis(
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
                        Percent::one(),
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

    /// Gemini 3b config
    pub fn gemini_3b_config() -> ExecutionChainSpec<GenesisConfig> {
        ExecutionChainSpec::from_genesis(
            // Name
            "Subspace Gemini 3b System Domain",
            // ID
            "subspace_gemini_3b_system_domain",
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
                        Percent::one(),
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
            Some("subspace-gemini-3b-system-domain"),
            None,
            // Properties
            Some(chain_spec_properties()),
            // Extensions
            None,
        )
    }

    /// X net 2 config
    pub fn x_net_2_config() -> ExecutionChainSpec<GenesisConfig> {
        ExecutionChainSpec::from_genesis(
            // Name
            "Subspace X-Net 2 Execution",
            // ID
            "subspace_x_net_2a_execution",
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
                        Percent::one(),
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
            Some("subspace-x-net-2a-execution"),
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

mod utils {
    use frame_support::traits::Get;
    use sc_service::Properties;
    use sp_core::crypto::AccountId32;
    use sp_core::{sr25519, Pair, Public};
    use sp_runtime::traits::IdentifyAccount;
    use sp_runtime::MultiSigner;
    use subspace_runtime::SS58Prefix;
    use subspace_runtime_primitives::DECIMAL_PLACES;

    /// Shared chain spec properties related to the coin.
    pub(crate) fn chain_spec_properties() -> Properties {
        let mut properties = Properties::new();

        properties.insert("ss58Format".into(), <SS58Prefix as Get<u16>>::get().into());
        properties.insert("tokenDecimals".into(), DECIMAL_PLACES.into());
        properties.insert("tokenSymbol".into(), "tSSC".into());

        properties
    }

    /// Get public key from keypair seed.
    pub(crate) fn get_public_key_from_seed<TPublic: Public>(
        seed: &'static str,
    ) -> <TPublic::Pair as Pair>::Public {
        TPublic::Pair::from_string(&format!("//{}", seed), None)
            .expect("Static values are valid; qed")
            .public()
    }

    /// Generate an account ID from seed.
    pub(crate) fn get_account_id_from_seed(seed: &'static str) -> AccountId32 {
        MultiSigner::from(get_public_key_from_seed::<sr25519::Public>(seed)).into_account()
    }
}

pub mod core_payments {
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

    use super::utils::{chain_spec_properties, get_account_id_from_seed};

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

    /// Gemini 3b chain spec
    pub fn gemini_3b_config() -> ExecutionChainSpec<GenesisConfig> {
        ExecutionChainSpec::from_genesis(
            // Name
            "Subspace Gemini 3b Core Payments Domain",
            // ID
            "subspace_gemini_3b_core_payments_domain",
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
            Some("subspace-gemini-3b-core-payments-domain"),
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

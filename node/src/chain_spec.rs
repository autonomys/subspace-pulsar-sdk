//! Subspace chain configurations.

use sc_service::ChainType;
use sc_subspace_chain_specs::SerializableChainSpec;
use sc_telemetry::TelemetryEndpoints;
use sdk_utils::chain_spec as utils;
use sp_core::crypto::{Ss58Codec, UncheckedFrom};
use subspace_runtime::{
    AllowAuthoringBy, BalancesConfig, GenesisConfig, RuntimeConfigsConfig, SubspaceConfig,
    SudoConfig, SystemConfig, VestingConfig, MILLISECS_PER_BLOCK, WASM_BINARY,
};
use subspace_runtime_primitives::{AccountId, Balance, BlockNumber, SSC};

const SUBSPACE_TELEMETRY_URL: &str = "wss://telemetry.subspace.network/submit/";
const GEMINI_3D_CHAIN_SPEC: &[u8] = include_bytes!("../res/chain-spec-raw-gemini-3d.json");
const DEVNET_CHAIN_SPEC: &[u8] = include_bytes!("../res/chain-spec-raw-devnet.json");

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
    enable_transfer: bool,
    confirmation_depth_k: u32,
}

/// Chain spec type for the subspace
pub type ChainSpec = SerializableChainSpec<GenesisConfig>;

/// Gemini 3d chain spec
pub fn gemini_3d() -> ChainSpec {
    ChainSpec::from_json_bytes(GEMINI_3D_CHAIN_SPEC).expect("Always valid")
}

/// Gemini 3d compiled chain spec
pub fn gemini_3d_compiled() -> ChainSpec {
    ChainSpec::from_genesis(
        // Name
        "Subspace Gemini 3d",
        // ID
        "subspace_gemini_3d",
        ChainType::Custom("Subspace Gemini 3d".to_string()),
        || {
            let sudo_account =
                AccountId::from_ss58check("5CZy4hcmaVZUMZLfB41v1eAKvtZ8W7axeWuDvwjhjPwfhAqt")
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
                        sp_consensus_subspace::FarmerPublicKey::unchecked_from(hex_literal::hex!(
                            "8aecbcf0b404590ddddc01ebacb205a562d12fdb5c2aa6a4035c1a20f23c9515"
                        )),
                    ),
                    enable_executor: true,
                    enable_transfer: false,
                    confirmation_depth_k: 100, // TODO: Proper value here
                },
            )
        },
        // Bootnodes
        vec![],
        // Telemetry
        Some(
            TelemetryEndpoints::new(vec![(SUBSPACE_TELEMETRY_URL.into(), 1)])
                .expect("Telemetry value is valid"),
        ),
        // Protocol ID
        Some("subspace-gemini-3d"),
        None,
        // Properties
        Some(utils::chain_spec_properties()),
        // Extensions
        None,
    )
}

/// Dev net raw configuration
pub fn devnet_config() -> ChainSpec {
    ChainSpec::from_json_bytes(DEVNET_CHAIN_SPEC).expect("Always valid")
}

/// Dev net compiled configuration
pub fn devnet_config_compiled() -> ChainSpec {
    ChainSpec::from_genesis(
        // Name
        "Subspace Dev network",
        // ID
        "subspace_devnet",
        ChainType::Custom("Testnet".to_string()),
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
                    enable_transfer: true,
                    confirmation_depth_k: 100, // TODO: Proper value here
                },
            )
        },
        // Bootnodes
        vec![],
        // Telemetry
        Some(
            TelemetryEndpoints::new(vec![(SUBSPACE_TELEMETRY_URL.into(), 1)])
                .expect("Telemetry value is valid"),
        ),
        // Protocol ID
        Some("subspace-devnet"),
        None,
        // Properties
        Some(utils::chain_spec_properties()),
        // Extensions
        None,
    )
}

/// New dev chain spec
pub fn dev_config() -> ChainSpec {
    let wasm_binary = WASM_BINARY.expect("Development wasm not available");

    ChainSpec::from_genesis(
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
                    enable_transfer: true,
                    enable_storage_access: false,
                    allow_authoring_by: AllowAuthoringBy::Anyone,
                    enable_executor: true,
                    confirmation_depth_k: 100,
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
        None,
    )
}

/// New local chain spec
pub fn local_config() -> ChainSpec {
    let wasm_binary = WASM_BINARY.expect("Development wasm not available");

    ChainSpec::from_genesis(
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
                    enable_transfer: true,
                    enable_storage_access: false,
                    allow_authoring_by: AllowAuthoringBy::Anyone,
                    enable_executor: true,
                    confirmation_depth_k: 1,
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
        None,
    )
}

/// Configure initial storage state for FRAME modules.
fn subspace_genesis_config(
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
        enable_transfer,
        confirmation_depth_k,
    } = genesis_params;

    GenesisConfig {
        domains: Default::default(),
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
        runtime_configs: RuntimeConfigsConfig {
            enable_executor,
            enable_transfer,
            confirmation_depth_k,
        },
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn test_chain_specs() {
        gemini_3d_compiled();
        gemini_3d();
        devnet_config_compiled();
        devnet_config();
        dev_config();
        local_config();
    }
}

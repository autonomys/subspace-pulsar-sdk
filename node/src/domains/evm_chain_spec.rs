//! System domain chain specs

use std::str::FromStr;

use evm_domain_runtime::{
    AccountId, BalancesConfig, EVMChainIdConfig, EVMConfig, GenesisConfig, MessengerConfig,
    Precompiles, SelfDomainIdConfig, SudoConfig, SystemConfig, WASM_BINARY,
};
use hex_literal::hex;
use once_cell::sync::OnceCell;
use sc_service::ChainType;
use sc_subspace_chain_specs::SerializableChainSpec;
use sdk_utils::chain_spec::{
    chain_spec_properties, get_account_id_from_seed, get_public_key_from_seed,
};
use sp_core::crypto::UncheckedFrom;
use sp_domains::{DomainId, DomainInstanceData, OperatorPublicKey, RuntimeType};
use sp_runtime::traits::Convert;
use subspace_runtime_primitives::SSC;

use crate::domains::utils::AccountId32ToAccountId20Converter;

/// Chain spec type for the system domain
pub type ChainSpec = SerializableChainSpec<GenesisConfig>;

pub enum SpecId {
    Dev,
    Gemini,
    DevNet,
    Local,
}

pub struct GenesisDomainParams {
    pub operator_signing_key: OperatorPublicKey,
}

pub fn create_domain_spec(
    domain_id: DomainId,
    chain_id: &str,
    domain_instance_data: DomainInstanceData,
) -> Result<ChainSpec, String> {
    let DomainInstanceData { runtime_type, runtime_code, raw_genesis_config } =
        domain_instance_data;

    match runtime_type {
        RuntimeType::Evm => {
            let mut genesis_config = match raw_genesis_config {
                Some(raw_genesis_config) =>
                    serde_json::from_slice(&raw_genesis_config).map_err(|_| {
                        "Failed to deserialize genesis config of the evm domain".to_string()
                    })?,
                None => GenesisConfig::default(),
            };
            genesis_config.system.code = runtime_code;
            genesis_config.self_domain_id.domain_id = Some(domain_id);
            let spec = load_chain_spec_with(chain_id, genesis_config)?;
            Ok(spec)
        }
    }
}

// HACK: `ChainSpec::from_genesis` is only allow to create hardcoded spec and
// `GenesisConfig` dosen't derive `Clone`, using global variable and
// serialization/deserialization to workaround these limits.
static GENESIS_CONFIG: OnceCell<Vec<u8>> = OnceCell::new();

fn load_chain_spec_with(spec_id: &str, genesis_config: GenesisConfig) -> Result<ChainSpec, String> {
    let result = GENESIS_CONFIG.set(
        serde_json::to_vec(&genesis_config).expect("Genesis config serialization never fails; qed"),
    );
    // We need this feature gate since as part of the integration tests we spin up
    // multiple nodes/farmers. #[cfg(test)] does not work with integration test.
    #[cfg(feature = "integration-test")]
    result.unwrap_or_else(|_prev_value| {
        tracing::warn!("Genesis config global variable was set twice.");
        ()
    });

    #[cfg(not(feature = "integration-test"))]
    result.expect("This function should only call once upon node initialization");

    let constructor = || {
        let raw_genesis_config = GENESIS_CONFIG.get().expect("Value just set; qed");
        serde_json::from_slice(raw_genesis_config)
            .expect("Genesis config deserialization never fails; qed")
    };

    let chain_spec = match spec_id {
        "dev" => development_config(constructor),
        "gemini-3e" => gemini_3e_config(constructor),
        "devnet" => devnet_config(constructor),
        "" | "local" => local_testnet_config(constructor),
        path => ChainSpec::from_json_file(std::path::PathBuf::from(path))?,
    };

    Ok(chain_spec)
}

/// Development keys that will be injected automatically on polkadotjs apps
fn get_dev_accounts() -> Vec<AccountId> {
    vec![
        // Alith key
        AccountId::from(hex!("f24FF3a9CF04c71Dbc94D0b566f7A27B94566cac")),
        // Baltathar key
        AccountId::from(hex!("3Cd0A705a2DC65e5b1E1205896BaA2be8A07c6e0")),
        // Charleth key
        AccountId::from(hex!("798d4Ba9baf0064Ec19eB4F0a1a45785ae9D6DFc")),
        // Dorothy
        AccountId::from(hex!("773539d4Ac0e786233D90A233654ccEE26a613D9")),
    ]
}

pub fn get_testnet_genesis_by_spec_id(spec_id: SpecId) -> (GenesisConfig, GenesisDomainParams) {
    match spec_id {
        SpecId::Dev => {
            let accounts = get_dev_accounts();
            (
                testnet_genesis(
                    accounts.clone(),
                    // Alith is Sudo
                    Some(accounts[0]),
                    vec![(
                        accounts[0],
                        AccountId32ToAccountId20Converter::convert(get_account_id_from_seed(
                            "Alice",
                        )),
                    )],
                    1000,
                ),
                GenesisDomainParams {
                    operator_signing_key: get_public_key_from_seed::<OperatorPublicKey>("Alice"),
                },
            )
        }
        SpecId::Gemini => {
            let sudo_account = AccountId::from_str("f31e60022e290708c17d6997c34de6a30d09438f")
                .expect("Invalid Sudo account");
            (
                testnet_genesis(
                    vec![
                        // Genesis operator
                        AccountId::from_str("2ac6c70c106138c8cd80da6b6a0e886b7eeee249")
                            .expect("Wrong executor account address"),
                        // Sudo account
                        sudo_account,
                    ],
                    Some(sudo_account),
                    Default::default(),
                    1002,
                ),
                GenesisDomainParams {
                    operator_signing_key: OperatorPublicKey::unchecked_from(hex!(
                        "aa3b05b4d649666723e099cf3bafc2f2c04160ebe0e16ddc82f72d6ed97c4b6b"
                    )),
                },
            )
        }
        SpecId::DevNet => {
            let sudo_account = AccountId::from_str("b66a91845249464309fad766fd0ece8144547736")
                .expect("Invalid Sudo account");
            (
                testnet_genesis(
                    vec![
                        // Genesis operator
                        AccountId::from_str("cfdf9f58d9e532c3807ce62a5489cb19cfa6942d")
                            .expect("Wrong executor account address"),
                        // Sudo account
                        sudo_account,
                    ],
                    Some(sudo_account),
                    vec![(
                        sudo_account,
                        AccountId::from_str("5b267fd1ba3ace6e3c3234f9576c49c877b5beb9")
                            .expect("Wrong relayer account address"),
                    )],
                    1003,
                ),
                GenesisDomainParams {
                    operator_signing_key: OperatorPublicKey::unchecked_from(hex!(
                        "aa3b05b4d649666723e099cf3bafc2f2c04160ebe0e16ddc82f72d6ed97c4b6b"
                    )),
                },
            )
        }
        SpecId::Local => {
            let accounts = get_dev_accounts();
            (
                testnet_genesis(
                    accounts.clone(),
                    // Alith is sudo
                    Some(accounts[0]),
                    vec![(accounts[0], accounts[0]), (accounts[1], accounts[1])],
                    1001,
                ),
                GenesisDomainParams {
                    operator_signing_key: get_public_key_from_seed::<OperatorPublicKey>("Alice"),
                },
            )
        }
    }
}

/// Development config
pub fn development_config<F: Fn() -> GenesisConfig + 'static + Send + Sync>(
    constructor: F,
) -> ChainSpec {
    ChainSpec::from_genesis(
        // Name
        "Development",
        // ID
        "system_domain_dev",
        ChainType::Development,
        constructor,
        vec![],
        None,
        None,
        None,
        Some(chain_spec_properties()),
        None,
    )
}

/// Local config
pub fn local_testnet_config<F: Fn() -> GenesisConfig + 'static + Send + Sync>(
    constructor: F,
) -> ChainSpec {
    ChainSpec::from_genesis(
        // Name
        "Local Testnet",
        // ID
        "system_domain_local_testnet",
        ChainType::Local,
        constructor,
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

/// Gemini 3d config
pub fn gemini_3e_config<F: Fn() -> GenesisConfig + 'static + Send + Sync>(
    constructor: F,
) -> ChainSpec {
    ChainSpec::from_genesis(
        // Name
        "Subspace Gemini 3d System Domain",
        // ID
        "subspace_gemini_3d_system_domain",
        ChainType::Live,
        constructor,
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
        None,
    )
}

pub fn devnet_config<F: Fn() -> GenesisConfig + 'static + Send + Sync>(
    constructor: F,
) -> ChainSpec {
    ChainSpec::from_genesis(
        // Name
        "Subspace Devnet System domain",
        // ID
        "subspace_devnet_system_domain",
        ChainType::Custom("Testnet".to_string()),
        constructor,
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
    maybe_sudo_account: Option<AccountId>,
    relayers: Vec<(AccountId, AccountId)>,
    chain_id: u64,
) -> GenesisConfig {
    // This is the simplest bytecode to revert without returning any data.
    // We will pre-deploy it under all of our precompiles to ensure they can be
    // called from within contracts.
    // (PUSH1 0x00 PUSH1 0x00 REVERT)
    let revert_bytecode = vec![0x60, 0x00, 0x60, 0x00, 0xFD];

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
        evm_chain_id: EVMChainIdConfig { chain_id },
        evm: EVMConfig {
            // We need _some_ code inserted at the precompile address so that
            // the evm will actually call the address.
            accounts: Precompiles::used_addresses()
                .into_iter()
                .map(|addr| {
                    (
                        addr,
                        fp_evm::GenesisAccount {
                            nonce: Default::default(),
                            balance: Default::default(),
                            storage: Default::default(),
                            code: revert_bytecode.clone(),
                        },
                    )
                })
                .collect(),
        },
        ethereum: Default::default(),
        base_fee: Default::default(),
        self_domain_id: SelfDomainIdConfig {
            // Id of the genesis domain
            domain_id: Some(DomainId::new(0)),
        },
    }
}

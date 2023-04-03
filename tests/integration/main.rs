pub mod common;
mod domains;
mod farmer;
mod node;

#[test]
fn test_pubkey_parse() {
    "5GrwvaEF5zXb26Fz9rcQpDWS57CtERHpNehXCPcNoHGKutQY".parse::<subspace_sdk::PublicKey>().unwrap();
}

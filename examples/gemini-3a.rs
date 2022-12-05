use subspace_sdk::node::{self, Node};
use tempfile::TempDir;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("runtime::subspace::executor=off".parse().unwrap())
                .add_directive("info".parse().unwrap()),
        )
        .init();

    let node_dir = TempDir::new()?;
    let node = Node::builder()
        .role(node::Role::Authority)
        .network(
            node::NetworkBuilder::new()
                .listen_addresses(vec![
                    "/ip6/::/tcp/30333".parse().unwrap(),
                    "/ip4/0.0.0.0/tcp/30333".parse().unwrap(),
                ])
                .enable_mdns(true),
        )
        .rpc(
            node::RpcBuilder::new()
                .http("127.0.0.1:9933".parse().unwrap())
                .ws("127.0.0.1:9944".parse().unwrap())
                .cors(vec![
                    "http://localhost:*".to_owned(),
                    "http://127.0.0.1:*".to_owned(),
                    "https://localhost:*".to_owned(),
                    "https://127.0.0.1:*".to_owned(),
                    "https://polkadot.js.org".to_owned(),
                ]),
        )
        .dsn(node::DsnBuilder::new().listen_addresses(vec![
            "/ip6/::/tcp/30433".parse().unwrap(),
            "/ip4/0.0.0.0/tcp/30433".parse().unwrap(),
        ]))
        .execution_strategy(node::ExecutionStrategy::AlwaysWasm)
        .offchain_worker(node::OffchainWorkerBuilder::new().enabled(true))
        .build(node_dir.as_ref(), node::chain_spec::gemini_3a().unwrap())
        .await?;

    node.sync().await?;
    tracing::info!("Node was synced!");

    node.close().await;
    Ok(())
}

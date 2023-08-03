use std::path::PathBuf;

use clap::Parser;
use futures::stream::StreamExt;
use subspace_sdk::farmer::CacheDescription;
use subspace_sdk::node::NetworkBuilder;
use subspace_sdk::{
    chain_spec, ByteSize, Farmer, MultiaddrWithPeerId, Node, PlotDescription, PublicKey,
};
use tempfile::TempDir;

#[derive(clap::Parser, Debug)]
enum Args {
    Farm {
        /// Path to the plot
        #[arg(short, long)]
        plot: PathBuf,

        /// Size of the plot
        #[arg(long)]
        plot_size: ByteSize,

        /// Path to the node directory
        #[arg(short, long)]
        node: PathBuf,

        /// Path to the chain spec
        #[arg(short, long)]
        spec: PathBuf,
    },
    Sync {
        /// Bootstrap nodes
        #[arg(short, long)]
        boot_nodes: Vec<MultiaddrWithPeerId>,

        /// Path to the chain spec
        #[arg(short, long)]
        spec: PathBuf,

        /// Total space pledged by farmer
        #[arg(short, long)]
        farmer_total_space_pledged: ByteSize,
    },
    GenerateSpec {
        path: PathBuf,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().init();

    let args = Args::parse();
    match args {
        Args::GenerateSpec { path } =>
            tokio::fs::write(path, serde_json::to_string_pretty(&chain_spec::dev_config())?).await?,
        Args::Farm { plot, plot_size, node, spec } => {
            let chain_spec = serde_json::from_str(&tokio::fs::read_to_string(spec).await?)?;
            let (plot_size, cache_size) =
                (ByteSize::b(plot_size.as_u64() * 9 / 10), ByteSize::b(plot_size.as_u64() / 10));
            let plots = [PlotDescription::new(plot.join("plot"), plot_size)];
            let farmer_total_space_pledged =
                plots.iter().map(|p| p.space_pledged.as_u64() as usize).sum::<usize>();
            let node = Node::builder()
                .network(
                    NetworkBuilder::new()
                        .listen_addresses(vec!["/ip4/127.0.0.1/tcp/0".parse().unwrap()])
                        .force_synced(true),
                )
                .force_authoring(true)
                .role(subspace_sdk::node::Role::Authority)
                .build(node, chain_spec, farmer_total_space_pledged)
                .await?;

            let _farmer: Farmer = Farmer::builder()
                .build(
                    PublicKey::from([13; 32]),
                    &node,
                    &plots,
                    CacheDescription::new(plot.join("cache"), cache_size)?,
                )
                .await?;

            let addr = node.listen_addresses().await?.into_iter().next().unwrap();
            tracing::info!(%addr, "Node listening at");

            node.subscribe_new_heads()
                .await?
                .for_each(|header| async move { tracing::info!(?header, "New block!") })
                .await;
        }
        Args::Sync { boot_nodes, spec, farmer_total_space_pledged } => {
            let node = TempDir::new()?;
            let chain_spec = serde_json::from_str(&tokio::fs::read_to_string(spec).await?)?;
            let node = Node::builder()
                .force_authoring(true)
                .role(subspace_sdk::node::Role::Authority)
                .network(NetworkBuilder::new().boot_nodes(boot_nodes))
                .build(node.as_ref(), chain_spec, farmer_total_space_pledged.as_u64() as usize)
                .await?;

            node.sync().await.unwrap();
            tracing::info!("Node was synced!");

            node.subscribe_new_heads()
                .await?
                .for_each(|header| async move { tracing::info!(?header, "New block!") })
                .await;
        }
    }

    Ok(())
}

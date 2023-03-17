use std::path::PathBuf;

use anyhow::Context;
use bytesize::ByteSize;
use clap::{Parser, Subcommand};
use futures::prelude::*;
use subspace_sdk::farmer::CacheDescription;
use subspace_sdk::node::{self, Node};
use subspace_sdk::{Farmer, PlotDescription, PublicKey};

#[derive(Subcommand, Debug)]
enum Chain {
    Gemini3C,
    Devnet,
}

/// Gemini 3c test binary
#[derive(Parser, Debug)]
#[command(author, version, about)]
pub struct Args {
    /// Set the chain
    #[command(subcommand)]
    chain: Chain,
    /// Should we run the executor?
    #[arg(short, long)]
    executor: bool,
    /// Address for farming rewards
    #[arg(short, long)]
    reward_address: PublicKey,
    /// Path for all data
    #[arg(short, long)]
    base_path: Option<PathBuf>,
    /// Size of the plot
    #[arg(short, long)]
    plot_size: ByteSize,
    /// Cache size
    #[arg(short, long, default_value_t = ByteSize::gib(1))]
    cache_size: ByteSize,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    fdlimit::raise_fd_limit();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("info".parse().unwrap()),
        )
        .init();

    let Args { chain, executor, reward_address, base_path, plot_size, cache_size } = Args::parse();
    let (base_path, _tmp_dir) = base_path.map(|x| (x, None)).unwrap_or_else(|| {
        let tmp = tempfile::tempdir().expect("Failed to create temporary directory");
        (tmp.as_ref().to_owned(), Some(tmp))
    });

    let node_dir = base_path.join("node");
    let node = match chain {
        Chain::Gemini3C => Node::gemini_3c().dsn(
            subspace_sdk::node::DsnBuilder::gemini_3c()
                .provider_storage_path(node_dir.join("provider_storage")),
        ),
        Chain::Devnet => Node::devnet().dsn(
            subspace_sdk::node::DsnBuilder::devnet()
                .provider_storage_path(node_dir.join("provider_storage")),
        ),
    }
    .role(node::Role::Authority);

    let node = if executor {
        node.system_domain(
            subspace_sdk::node::domains::ConfigBuilder::new()
                .core(subspace_sdk::node::domains::core::ConfigBuilder::new().build()),
        )
    } else {
        node
    }
    .build(
        &node_dir,
        match chain {
            Chain::Gemini3C => node::chain_spec::gemini_3c().unwrap(),
            Chain::Devnet => node::chain_spec::devnet_config().unwrap(),
        },
    )
    .await?;

    tokio::select! {
        result = node.sync() => result?,
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("Exitting...");
            return node.close().await.context("Failed to close node")
        }
    }
    tracing::info!("Node was synced!");

    let farmer = Farmer::builder()
        .build(
            reward_address,
            &node,
            &[PlotDescription::new(base_path.join("plot"), plot_size)
                .context("Failed to create a plot")?],
            CacheDescription::new(base_path.join("cache"), cache_size).unwrap(),
        )
        .await?;
    let plot = farmer.iter_plots().await.next().unwrap();

    let subscriptions = async move {
        plot.subscribe_initial_plotting_progress()
            .await
            .for_each(|progress| async move {
                tracing::info!(?progress, "Plotting!");
            })
            .await;
        plot.subscribe_new_solutions()
            .await
            .for_each(|_| async move {
                tracing::info!("Farmed another solution!");
            })
            .await;
    };

    tokio::select! {
        _ = subscriptions => {},
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("Exitting...");
            return Ok(())
        }
    }

    node.close().await.context("Failed to close node")?;
    farmer.close().await.context("Failed to close farmer")?;

    Ok(())
}

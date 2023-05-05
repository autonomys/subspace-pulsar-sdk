use std::path::PathBuf;

use anyhow::Context;
use clap::{Parser, ValueEnum};
use futures::prelude::*;
use subspace_sdk::farmer::CacheDescription;
use subspace_sdk::node::{self, Event, Node, RewardsEvent, SubspaceEvent};
use subspace_sdk::{ByteSize, Farmer, PlotDescription, PublicKey};
use tracing_subscriber::prelude::*;

#[cfg(all(
    target_arch = "x86_64",
    target_vendor = "unknown",
    target_os = "linux",
    target_env = "gnu"
))]
#[global_allocator]
static GLOBAL: jemallocator::Jemalloc = jemallocator::Jemalloc;

#[derive(ValueEnum, Debug, Clone)]
enum Chain {
    Gemini3d,
    Devnet,
    Dev,
}

#[cfg(feature = "executor")]
#[derive(ValueEnum, Debug, Clone)]
enum Domain {
    #[cfg(feature = "core-payments")]
    Payments,
    #[cfg(feature = "eth-relayer")]
    EthRelayer,
    #[cfg(feature = "core-evm")]
    Evm,
}

/// Mini farmer
#[derive(Parser, Debug)]
#[command(author, version, about)]
pub struct Args {
    /// Set the chain
    #[arg(value_enum)]
    chain: Chain,
    #[cfg(feature = "executor")]
    /// Run executor with specified domain
    #[arg(short, long)]
    domain: Option<Domain>,
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

    #[cfg(tokio_unstable)]
    let registry = tracing_subscriber::registry().with(console_subscriber::spawn());
    #[cfg(not(tokio_unstable))]
    let registry = tracing_subscriber::registry();

    registry
        .with(tracing_subscriber::fmt::layer())
        .with(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("info".parse().unwrap()),
        )
        .init();

    let Args {
        chain,
        #[cfg(feature = "executor")]
        domain,
        reward_address,
        base_path,
        plot_size,
        cache_size,
    } = Args::parse();
    let (base_path, _tmp_dir) = base_path.map(|x| (x, None)).unwrap_or_else(|| {
        let tmp = tempfile::tempdir().expect("Failed to create temporary directory");
        (tmp.as_ref().to_owned(), Some(tmp))
    });

    let node_dir = base_path.join("node");
    let node = match chain {
        Chain::Gemini3d => Node::gemini_3d().dsn(
            subspace_sdk::node::DsnBuilder::gemini_3d()
                .provider_storage_path(node_dir.join("provider_storage")),
        ),
        Chain::Devnet => Node::devnet().dsn(
            subspace_sdk::node::DsnBuilder::devnet()
                .provider_storage_path(node_dir.join("provider_storage")),
        ),
        Chain::Dev => Node::dev().dsn(
            subspace_sdk::node::DsnBuilder::dev()
                .provider_storage_path(node_dir.join("provider_storage")),
        ),
    }
    .role(node::Role::Authority);

    #[cfg(feature = "executor")]
    let node = match domain {
        Some(domain) => node.system_domain({
            use subspace_sdk::node::domains;

            let system_domain = domains::ConfigBuilder::new()
                .rpc(
                    subspace_sdk::node::RpcBuilder::new()
                        .http("127.0.0.1:9990".parse().unwrap())
                        .ws("127.0.0.1:9991".parse().unwrap()),
                )
                .role(node::Role::Authority);

            match domain {
                #[cfg(feature = "core-payments")]
                Domain::Payments => system_domain.core_payments(
                    domains::core_payments::ConfigBuilder::new()
                        .rpc(
                            subspace_sdk::node::RpcBuilder::new()
                                .http("127.0.0.1:9992".parse().unwrap())
                                .ws("127.0.0.1:9993".parse().unwrap()),
                        )
                        .role(node::Role::Authority)
                        .build(),
                ),
                #[cfg(feature = "eth-relayer")]
                Domain::EthRelayer => system_domain.eth_relayer(
                    domains::eth_relayer::ConfigBuilder::new()
                        .rpc(
                            subspace_sdk::node::RpcBuilder::new()
                                .http("127.0.0.1:9992".parse().unwrap())
                                .ws("127.0.0.1:9993".parse().unwrap()),
                        )
                        .role(node::Role::Authority)
                        .build(),
                ),
                #[cfg(feature = "core-evm")]
                Domain::Evm => system_domain.evm(
                    domains::evm::ConfigBuilder::new()
                        .rpc(
                            subspace_sdk::node::RpcBuilder::new()
                                .http("127.0.0.1:9992".parse().unwrap())
                                .ws("127.0.0.1:9993".parse().unwrap()),
                        )
                        .role(node::Role::Authority)
                        .build(),
                ),
            }
        }),
        None => node,
    };

    let node = node
        .build(
            &node_dir,
            match chain {
                Chain::Gemini3d => node::chain_spec::gemini_3d(),
                Chain::Devnet => node::chain_spec::devnet_config(),
                Chain::Dev => node::chain_spec::dev_config(),
            },
        )
        .await?;

    let sync = if !matches!(chain, Chain::Dev) {
        futures::future::Either::Left(node.sync())
    } else {
        futures::future::Either::Right(futures::future::ok(()))
    };

    tokio::select! {
        result = sync => result?,
        _ = tokio::signal::ctrl_c() => {
            tracing::error!("Exitting...");
            return node.close().await.context("Failed to close node")
        }
    }
    tracing::error!("Node was synced!");

    let farmer = Farmer::builder()
        .build(
            reward_address,
            &node,
            &[PlotDescription::new(base_path.join("plot"), plot_size)],
            CacheDescription::new(base_path.join("cache"), cache_size).unwrap(),
        )
        .await?;

    tokio::spawn({
        let initial_plotting =
            farmer.iter_plots().await.next().unwrap().subscribe_initial_plotting_progress().await;
        async move {
            initial_plotting
                .for_each(|progress| async move {
                    tracing::error!(?progress, "Plotting!");
                })
                .await;
            tracing::error!("Finished initial plotting!");
        }
    });

    let rewards_sub = {
        let node = &node;

        async move {
            let mut new_blocks = node.subscribe_finalized_heads().await?;
            while let Some(header) = new_blocks.next().await {
                let events = node.get_events(Some(header.hash)).await?;

                for event in events {
                    match event {
                        Event::Rewards(
                            RewardsEvent::VoteReward { reward, voter: author }
                            | RewardsEvent::BlockReward { reward, block_author: author },
                        ) if author == reward_address.into() =>
                            tracing::error!(%reward, "Received a reward!"),
                        Event::Subspace(SubspaceEvent::FarmerVote {
                            reward_address: author,
                            height: block_number,
                            ..
                        }) if author == reward_address.into() =>
                            tracing::error!(block_number, "Vote counted for block"),
                        _ => (),
                    };
                }

                if let Some(pre_digest) = header.pre_digest {
                    if pre_digest.solution.reward_address == reward_address {
                        tracing::error!("We authored a block");
                    }
                }
            }

            anyhow::Ok(())
        }
    };

    tokio::select! {
        _ = rewards_sub => {},
        _ = tokio::signal::ctrl_c() => {
            tracing::error!("Exitting...");
        }
    }

    node.close().await.context("Failed to close node")?;
    farmer.close().await.context("Failed to close farmer")?;

    Ok(())
}

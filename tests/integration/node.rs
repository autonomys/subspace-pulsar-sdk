use futures::prelude::*;
use subspace_sdk::farmer::{CacheDescription, Farmer, PlotDescription};
use subspace_sdk::node::*;
use tempfile::TempDir;
use tracing_futures::Instrument;

async fn sync_block_inner() {
    crate::common::setup();

    let dir = TempDir::new().unwrap();
    let chain = chain_spec::dev_config();
    let node = Node::dev()
        .role(Role::Authority)
        .network(
            NetworkBuilder::dev().listen_addresses(vec!["/ip4/127.0.0.1/tcp/0".parse().unwrap()]),
        )
        .role(Role::Authority)
        .build(dir.path(), chain.clone())
        .await
        .unwrap();
    let (plot_dir, cache_dir) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let farmer = Farmer::builder()
        .build(
            Default::default(),
            &node,
            &[PlotDescription::minimal(plot_dir.as_ref())],
            CacheDescription::minimal(cache_dir.as_ref()),
        )
        .await
        .unwrap();

    let farm_blocks = 4;

    node.subscribe_new_blocks()
        .await
        .unwrap()
        .skip_while(|notification| futures::future::ready(notification.number < farm_blocks))
        .next()
        .await
        .unwrap();

    farmer.close().await.unwrap();

    let dir = TempDir::new().unwrap();
    let other_node = Node::dev()
        .network(
            NetworkBuilder::dev()
                .force_synced(false)
                .boot_nodes(node.listen_addresses().await.unwrap()),
        )
        .build(dir.path(), chain)
        .await
        .unwrap();

    other_node.subscribe_syncing_progress().await.unwrap().for_each(|_| async {}).await;
    assert_eq!(other_node.get_info().await.unwrap().best_block.1, farm_blocks);

    node.close().await.unwrap();
    other_node.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[cfg_attr(any(tarpaulin, not(target_os = "linux")), ignore = "Slow tests are run only on linux")]
async fn sync_block() {
    tokio::time::timeout(std::time::Duration::from_secs(60 * 60), sync_block_inner()).await.unwrap()
}

async fn sync_plot_inner() {
    crate::common::setup();

    let node_span = tracing::trace_span!("node 1");
    let dir = TempDir::new().unwrap();
    let chain = chain_spec::dev_config();
    let node = Node::dev()
        .dsn(DsnBuilder::dev().listen_addresses(vec!["/ip4/127.0.0.1/tcp/0".parse().unwrap()]))
        .network(
            NetworkBuilder::dev().listen_addresses(vec!["/ip4/127.0.0.1/tcp/0".parse().unwrap()]),
        )
        .role(Role::Authority)
        .build(dir.path(), chain.clone())
        .instrument(node_span.clone())
        .await
        .unwrap();
    let (plot_dir, cache_dir) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let farmer = Farmer::builder()
        .build(
            Default::default(),
            &node,
            &[PlotDescription::minimal(plot_dir.as_ref())],
            CacheDescription::minimal(cache_dir.as_ref()),
        )
        .instrument(node_span.clone())
        .await
        .unwrap();

    let farm_blocks = 4;

    node.subscribe_new_blocks()
        .await
        .unwrap()
        .skip_while(|notification| futures::future::ready(notification.number < farm_blocks))
        .next()
        .await
        .unwrap();

    let other_node_span = tracing::trace_span!("node 2");
    let dir = TempDir::new().unwrap();
    let other_node = Node::dev()
        .dsn(DsnBuilder::dev().boot_nodes(node.dsn_listen_addresses().await.unwrap()))
        .network(
            NetworkBuilder::dev()
                .force_synced(false)
                .boot_nodes(node.listen_addresses().await.unwrap()),
        )
        .build(dir.path(), chain)
        .instrument(other_node_span.clone())
        .await
        .unwrap();

    while other_node.get_info().await.unwrap().best_block.1
        < node.get_info().await.unwrap().best_block.1
    {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }

    let (plot_dir, cache_dir) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let other_farmer = Farmer::builder()
        .build(
            Default::default(),
            &node,
            &[PlotDescription::minimal(plot_dir.as_ref())],
            CacheDescription::minimal(cache_dir.as_ref()),
        )
        .instrument(other_node_span.clone())
        .await
        .unwrap();

    let plot = other_farmer.iter_plots().await.next().unwrap();
    plot.subscribe_initial_plotting_progress().await.for_each(|_| async {}).await;
    farmer.close().await.unwrap();

    plot.subscribe_new_solutions().await.next().await.expect("Solution stream never ends");

    node.close().await.unwrap();
    other_node.close().await.unwrap();
    other_farmer.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[cfg_attr(any(tarpaulin, not(target_os = "linux")), ignore = "Slow tests are run only on linux")]
async fn sync_plot() {
    tokio::time::timeout(std::time::Duration::from_secs(60 * 60), sync_plot_inner()).await.unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn node_restart() {
    crate::common::setup();
    let dir = TempDir::new().unwrap();

    for i in 0..4 {
        tracing::error!(i, "Running new node");
        Node::dev()
            .build(dir.path(), chain_spec::dev_config())
            .await
            .unwrap()
            .close()
            .await
            .unwrap();
    }
}

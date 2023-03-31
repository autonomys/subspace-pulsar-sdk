use futures::prelude::*;
use subspace_sdk::farmer::CacheDescription;
use subspace_sdk::node::domains::core::*;
use subspace_sdk::node::{chain_spec, domains, Role};
use subspace_sdk::{Farmer, Node, PlotDescription};
use tempfile::TempDir;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn core_start() {
    crate::common::setup();

    let dir = TempDir::new().unwrap();
    let core = ConfigBuilder::new().build();
    let node = Node::dev()
        .role(Role::Authority)
        .system_domain(domains::ConfigBuilder::new().core(core))
        .build(dir.path(), chain_spec::dev_config())
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

    node.system_domain()
        .unwrap()
        .core()
        .unwrap()
        .subscribe_new_blocks()
        .await
        .unwrap()
        .next()
        .await
        .unwrap();

    farmer.close().await.unwrap();
    node.close().await.unwrap();
}

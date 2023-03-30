use bytesize::ByteSize;
use futures::prelude::*;
use subspace_sdk::farmer::*;
use subspace_sdk::node::{chain_spec, Node, Role};
use tempfile::TempDir;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_get_info() {
    crate::common::setup();

    let dir = TempDir::new().unwrap();
    let node = Node::dev()
        .role(Role::Authority)
        .build(dir.path(), chain_spec::dev_config())
        .await
        .unwrap();
    let plot_dir = TempDir::new().unwrap();
    let cache_dir = TempDir::new().unwrap();
    let farmer = Farmer::builder()
        .build(
            Default::default(),
            &node,
            &[PlotDescription::minimal(plot_dir.as_ref())],
            CacheDescription::minimal(cache_dir.as_ref()),
        )
        .await
        .unwrap();

    let Info { reward_address, plots_info, .. } = farmer.get_info().await.unwrap();
    assert_eq!(reward_address, Default::default());
    assert_eq!(plots_info.len(), 1);
    assert_eq!(plots_info[plot_dir.as_ref()].allocated_space, PlotDescription::MIN_SIZE);

    farmer.close().await.unwrap();
    node.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_track_progress() {
    crate::common::setup();

    let dir = TempDir::new().unwrap();
    let node = Node::dev()
        .role(Role::Authority)
        .build(dir.path(), chain_spec::dev_config())
        .await
        .unwrap();
    let (plot_dir, cache_dir) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let n_sectors = 2;
    let farmer = Farmer::builder()
        .build(
            Default::default(),
            &node,
            &[PlotDescription::new(
                plot_dir.as_ref(),
                ByteSize::b(PlotDescription::MIN_SIZE.as_u64() * n_sectors),
            )
            .unwrap()],
            CacheDescription::minimal(cache_dir.as_ref()),
        )
        .await
        .unwrap();

    let progress = farmer
        .iter_plots()
        .await
        .next()
        .unwrap()
        .subscribe_initial_plotting_progress()
        .await
        .take(n_sectors as usize)
        .collect::<Vec<_>>()
        .await;
    assert_eq!(progress.len(), n_sectors as usize);

    farmer.close().await.unwrap();
    node.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_new_solution() {
    crate::common::setup();

    let dir = TempDir::new().unwrap();
    let node = Node::dev()
        .role(Role::Authority)
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

    farmer
        .iter_plots()
        .await
        .next()
        .unwrap()
        .subscribe_new_solutions()
        .await
        .next()
        .await
        .expect("Farmer should send new solutions");

    farmer.close().await.unwrap();
    node.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_progress_restart() {
    crate::common::setup();

    let dir = TempDir::new().unwrap();
    let node = Node::dev()
        .role(Role::Authority)
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

    let plot = farmer.iter_plots().await.next().unwrap();

    plot.subscribe_initial_plotting_progress().await.for_each(|_| async {}).await;

    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        plot.subscribe_initial_plotting_progress().await.for_each(|_| async {}),
    )
    .await
    .unwrap();

    farmer.close().await.unwrap();
    node.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn test_farmer_restart() {
    crate::common::setup();

    let dir = TempDir::new().unwrap();
    let node = Node::dev()
        .role(Role::Authority)
        .build(dir.path(), chain_spec::dev_config())
        .await
        .unwrap();

    for _ in 0..10 {
        Farmer::builder()
            .build(
                Default::default(),
                &node,
                &[PlotDescription::minimal(dir.path().join("plot"))],
                CacheDescription::minimal(dir.path().join("cache")),
            )
            .await
            .unwrap()
            .close()
            .await
            .unwrap();
    }

    node.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn test_farmer_drop() {
    crate::common::setup();

    let dir = TempDir::new().unwrap();
    let node = Node::dev()
        .role(Role::Authority)
        .build(dir.path(), chain_spec::dev_config())
        .await
        .unwrap();

    drop(
        Farmer::builder()
            .build(
                Default::default(),
                &node,
                &[PlotDescription::minimal(dir.path().join("plot"))],
                CacheDescription::minimal(dir.path().join("cache")),
            )
            .await
            .unwrap(),
    )
}

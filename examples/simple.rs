use bytesize::ByteSize;
use subspace_sdk::{
    chain_spec, farmer::CacheDescription, Farmer, Node, PlotDescription, PublicKey,
};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().init();

    let node = Node::builder()
        .force_authoring(true)
        .role(subspace_sdk::node::Role::Authority)
        .build("node", chain_spec::dev_config().unwrap())
        .await
        .unwrap();

    let plots = [PlotDescription::new("plot", ByteSize::mb(100))];
    let farmer: Farmer = Farmer::builder()
        .dsn(
            subspace_sdk::farmer::DsnBuilder::new()
                .listen_on(vec!["/ip4/0.0.0.0/tcp/40333".parse().unwrap()]),
        )
        .build(
            PublicKey::from([13; 32]),
            node.clone(),
            &plots,
            CacheDescription::new("cache", ByteSize::mib(100)).unwrap(),
        )
        .await
        .expect("Failed to init a farmer");

    tracing::error!("Waiting for plotting to complete");
    tokio::time::sleep(std::time::Duration::from_secs(30)).await;

    {
        use futures::StreamExt;
        use subspace_farmer::RpcClient;

        let mut slot_info_sub = node.subscribe_slot_info().await.unwrap().take(10);
        while let Some(slot_info) = slot_info_sub.next().await {
            tracing::info!(?slot_info, "New slot");
        }
    }

    farmer.close().await.unwrap();
    node.close().await;

    for plot in plots {
        plot.wipe().await.unwrap();
    }
    Node::wipe("node").await.unwrap();
}

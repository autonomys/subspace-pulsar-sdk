use bytesize::ByteSize;
use subspace_sdk::{Farmer, Network, Node, NodeMode, PlotDescription, PublicKey};

#[tokio::main]
async fn main() {
    let mut node: Node = Node::builder()
        .mode(NodeMode::Full)
        .network(Network::Gemini2a)
        .name("i1i1")
        .port(1337)
        .directory("node")
        .build()
        .await
        .expect("Failed to init a node");

    node.sync().await;

    let reward_address = PublicKey::from([0; 32]);
    let mut farmer: Farmer = Farmer::builder()
        .node(node.clone())
        .plot(PlotDescription::new("plot", ByteSize::gb(10)))
        .ws_rpc("127.0.0.1:9955".parse().unwrap())
        .listen_on("/ip4/0.0.0.0/tcp/40333".parse().unwrap())
        .reward_address(reward_address)
        .build()
        .await
        .expect("Failed to init a farmer");

    farmer.sync().await;

    farmer
        .on_solution(|solution| async move {
            eprintln!("Found solution: {solution:?}");
        })
        .await;
    node.on_block(|block| async move {
        eprintln!("New block: {block:?}");
    })
    .await;

    farmer.start_farming().await;

    dbg!(node.get_info().await);
    dbg!(farmer.get_info().await);

    farmer.stop_farming().await;
    farmer.close().await;
    node.close().await;

    // Restarting
    let mut node = Node::builder()
        .mode(NodeMode::Full)
        .network(Network::Gemini2a)
        .directory("node")
        .build()
        .await
        .expect("Failed to init a node");
    node.sync().await;

    let mut farmer = Farmer::builder()
        .node(node.clone())
        .plot(PlotDescription::new("plot", ByteSize::gb(10)))
        .reward_address(reward_address)
        .build()
        .await
        .expect("Failed to init a farmer");

    farmer.sync().await;
    farmer.start_farming().await;

    // Delete everything
    for plot in farmer.plots().await {
        plot.delete().await;
    }
    farmer.wipe().await;
    node.wipe().await;
}

use bytesize::ByteSize;
use futures::StreamExt;
use subspace_sdk::{Chain, Farmer, Node, NodeMode, PlotDescription, PublicKey};

#[tokio::main]
async fn main() {
    let mut node: Node = Node::builder()
        .mode(NodeMode::Full)
        .chain(Chain::Gemini2a)
        .name("i1i1")
        .build("node")
        .await
        .expect("Failed to init a node");

    node.sync().await;

    let reward_address = PublicKey::from([0; 32]);
    let plots = [PlotDescription::new("plot", ByteSize::gb(10))];
    let mut farmer: Farmer = Farmer::builder()
        // .ws_rpc("127.0.0.1:9955".parse().unwrap())
        // .listen_on("/ip4/0.0.0.0/tcp/40333".parse().unwrap())
        .build(reward_address, node.clone(), &plots)
        .await
        .expect("Failed to init a farmer");

    farmer.sync().await;

    tokio::spawn({
        let mut solutions = farmer.subscribe_solutions().await;
        async move {
            while let Some(solution) = solutions.next().await {
                eprintln!("Found solution: {solution:?}");
            }
        }
    });
    tokio::spawn({
        let mut new_blocks = node.subscribe_new_blocks().await;
        async move {
            while let Some(block) = new_blocks.next().await {
                eprintln!("New block: {block:?}");
            }
        }
    });

    farmer.start_farming().await;

    dbg!(node.get_info().await);
    dbg!(farmer.get_info().await);

    farmer.stop_farming().await;
    farmer.close().await.expect("Failed to close the farmer");
    node.close().await;

    // Restarting
    let mut node = Node::builder()
        .mode(NodeMode::Full)
        .chain(Chain::Gemini2a)
        .build("node")
        .await
        .expect("Failed to init a node");
    node.sync().await;

    let mut farmer = Farmer::builder()
        .build(
            reward_address,
            node.clone(),
            &[PlotDescription::new("plot", ByteSize::gb(10))],
        )
        .await
        .expect("Failed to init a farmer");

    farmer.sync().await;
    farmer.start_farming().await;

    farmer.close().await.unwrap();
    node.close().await;

    // Delete everything
    for plot in plots {
        plot.wipe().await.unwrap();
    }
    Node::wipe("node").await.unwrap();
}

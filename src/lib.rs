pub mod farmer;
pub mod node;

pub use farmer::{
    Builder as FarmerBuilder, Farmer, Info as NodeInfo, Plot, PlotDescription, Solution,
};
pub use node::{
    chain_spec, Builder as NodeBuilder, Info as FarmerInfo, Mode as NodeMode, Node,
};
pub use subspace_core_primitives::PublicKey;

#[cfg(test)]
mod tests {
    use futures::StreamExt;
    use subspace_farmer::RpcClient;
    use tempdir::TempDir;

    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn test_integration() {
        let dir = TempDir::new("test").unwrap();
        let node = Node::builder()
            .force_authoring(true)
            .role(sc_service::Role::Authority)
            .build(dir, node::chain_spec::dev_config().unwrap(),)
            .await
            .unwrap();

        let mut slot_info_sub = node.subscribe_slot_info().await.unwrap();

        let dir = TempDir::new("test").unwrap();
        let plot_descriptions = [PlotDescription::new(dir.path(), bytesize::ByteSize::gb(1))];
        let _farmer = Farmer::builder()
            .build(Default::default(), node.clone(), &plot_descriptions)
            .await;

        // New slots arrive at each block. So basically we wait for 3 blocks to produce
        for _ in 0..3 {
            assert!(slot_info_sub.next().await.is_some());
        }
    }
}

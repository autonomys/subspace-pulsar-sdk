pub mod farmer;
pub mod node;

use std::path::PathBuf;

pub use farmer::{
    Builder as FarmerBuilder, Farmer, Info as NodeInfo, Plot, PlotDescription, Solution,
};
pub use node::{Builder as NodeBuilder, Chain, Info as FarmerInfo, Mode as NodeMode, Node};
pub use subspace_core_primitives::PublicKey;

#[derive(Default)]
#[non_exhaustive]
pub enum Directory {
    #[default]
    Default,
    Tmp,
    Custom(PathBuf),
}

impl<P: Into<PathBuf>> From<P> for Directory {
    fn from(path: P) -> Self {
        Self::Custom(path.into())
    }
}

#[cfg(test)]
mod tests {
    use futures::StreamExt;
    use subspace_farmer::RpcClient;

    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn test_integration() {
        let node = Node::builder()
            .at_directory(Directory::Tmp)
            .chain(Chain::Custom(Box::new(
                subspace_node::chain_spec::dev_config().unwrap(),
            )))
            .force_authoring(true)
            .role(sc_service::Role::Authority)
            .build()
            .await
            .unwrap();

        let mut slot_info_sub = node.subscribe_slot_info().await.unwrap();

        let plot_descriptions = [PlotDescription::with_tempdir(bytesize::ByteSize::gb(1)).unwrap()];
        let _farmer = Farmer::builder().build(Default::default(), node.clone(), &plot_descriptions);

        assert!(slot_info_sub.next().await.is_some());
    }
}

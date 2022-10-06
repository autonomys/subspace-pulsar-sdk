pub mod farmer;
pub mod node;

use std::path::PathBuf;

pub use farmer::{
    Builder as FarmerBuilder, Farmer, Info as NodeInfo, Plot, PlotDescription, Solution,
};
pub use node::{Builder as NodeBuilder, Info as FarmerInfo, Mode as NodeMode, Network, Node};
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

pub mod farmer;
pub mod node;

pub use farmer::{
    Builder as FarmerBuilder, Farmer, Info as NodeInfo, Plot, PlotDescription, Solution,
};
pub use node::{Builder as NodeBuilder, Info as FarmerInfo, Mode as NodeMode, Network, Node};
pub use subspace_core_primitives::PublicKey;

#[derive(Default)]
#[non_exhaustive]
enum Directory {
    #[default]
    Default,
    Tmp,
    Custom(std::path::PathBuf),
}

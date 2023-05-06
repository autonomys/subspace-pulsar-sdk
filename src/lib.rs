//! Subspace SDK for easy running of both Subspace node and farmer

#![warn(
    missing_docs,
    clippy::dbg_macro,
    clippy::unwrap_used,
    clippy::disallowed_types,
    unused_crate_dependencies,
    unused_features
)]
#![feature(type_changing_struct_update, concat_idents, const_option)]

/// Module related to the farmer
pub mod farmer;
/// Module related to the node
pub mod node;

pub use farmer::{Builder as FarmerBuilder, Farmer, Info as NodeInfo, Plot, PlotDescription};
pub use node::{chain_spec, Builder as NodeBuilder, Info as FarmerInfo, Node};
pub use sdk_utils::{ByteSize, Multiaddr, MultiaddrWithPeerId, PublicKey, Ss58ParsingError};

type PosTable = subspace_proof_of_space::chia::ChiaTable;

//! Subspace SDK for easy running of both Subspace node and farmer

#![warn(missing_docs, clippy::dbg_macro, clippy::unwrap_used, clippy::disallowed_types)]
#![feature(type_changing_struct_update, concat_idents, const_option)]

pub(crate) mod dsn;
/// Module related to the farmer
pub mod farmer;
/// Module related to the node
pub mod node;

pub use farmer::{Builder as FarmerBuilder, Farmer, Info as NodeInfo, Plot, PlotDescription};
pub use node::{chain_spec, Builder as NodeBuilder, Info as FarmerInfo, Node};
pub use sdk_utils::{ByteSize, Multiaddr, MultiaddrWithPeerId, PublicKey, Ss58ParsingError};

#[doc(hidden)]
#[macro_export]
macro_rules! generate_builder {
    ( $name:ident ) => {
        impl concat_idents!($name, Builder) {
            /// Constructor
            pub fn new() -> Self {
                Self::default()
            }

            #[doc = concat!("Build ", stringify!($name))]
            pub fn build(&self) -> $name {
                self._build().expect("Infallible")
            }
        }

        impl From<concat_idents!($name, Builder)> for $name {
            fn from(value: concat_idents!($name, Builder)) -> Self {
                value.build()
            }
        }
    };
    ( $name:ident, $($rest:ident),+ ) => {
        $crate::generate_builder!($name);
        $crate::generate_builder!($($rest),+);
    };
}

type PosTable = subspace_proof_of_space::chia::ChiaTable;

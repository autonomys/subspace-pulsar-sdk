use std::sync::Arc;

use derive_builder::Builder;
use derive_more::{Deref, DerefMut};
use sc_network_common::config::MultiaddrWithPeerId;
use subspace_sdk::farmer::{CacheDescription, PlotDescription};
use subspace_sdk::node::{chain_spec, ChainSpec, DsnBuilder, NetworkBuilder, Role};
use tempfile::TempDir;

pub fn setup() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            "debug,parity-db=info,cranelift_codegen=info,wasmtime_cranelift=info,\
             subspace_sdk=trace,subspace_farmer=trace,subspace_service=trace,\
             subspace_farmer::utils::parity_db_store=debug,trie-cache=info,wasm_overrides=info,\
             libp2p_gossipsub::behaviour=info,wasmtime_jit=info,wasm-runtime=info"
                .parse::<tracing_subscriber::EnvFilter>()
                .expect("Env filter directives are correct"),
        )
        .with_test_writer()
        .try_init();
}

#[derive(Builder)]
#[builder(pattern = "immutable", build_fn(private, name = "_build"), name = "NodeBuilder")]
pub struct InnerNode {
    #[builder(default)]
    not_force_synced: bool,
    #[builder(default)]
    boot_nodes: Vec<MultiaddrWithPeerId>,
    #[builder(default)]
    dsn_boot_nodes: Vec<MultiaddrWithPeerId>,
    #[builder(default)]
    not_authority: bool,
    #[builder(default = "chain_spec::dev_config()")]
    chain: ChainSpec,
    #[builder(default = "TempDir::new().map(Arc::new).unwrap()")]
    path: Arc<TempDir>,
}

#[derive(Deref, DerefMut)]
pub struct Node {
    #[deref]
    #[deref_mut]
    node: subspace_sdk::Node,
    pub path: Arc<TempDir>,
    pub chain: ChainSpec,
}

impl NodeBuilder {
    pub async fn build(self) -> Node {
        let InnerNode { not_force_synced, boot_nodes, dsn_boot_nodes, not_authority, chain, path } =
            self._build().expect("Infallible");
        let node = subspace_sdk::Node::dev()
            .dsn(
                DsnBuilder::dev()
                    .listen_addresses(vec!["/ip4/127.0.0.1/tcp/0".parse().unwrap()])
                    .boot_nodes(dsn_boot_nodes),
            )
            .network(
                NetworkBuilder::dev()
                    .force_synced(!not_force_synced)
                    .listen_addresses(vec!["/ip4/127.0.0.1/tcp/0".parse().unwrap()])
                    .boot_nodes(boot_nodes),
            )
            .role(if not_authority { Role::Full } else { Role::Authority })
            .build(path.path().join("node"), chain.clone())
            .await
            .unwrap();
        Node { node, path, chain }
    }
}

impl Node {
    pub fn dev() -> NodeBuilder {
        NodeBuilder::default()
    }

    pub fn path(&self) -> Arc<TempDir> {
        Arc::clone(&self.path)
    }

    pub async fn close(self) {
        self.node.close().await.unwrap()
    }
}

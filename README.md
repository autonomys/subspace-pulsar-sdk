# Subspace-SDK

[TODO]: <> (Add docs-rs label (should we generate and host our own one?))

A library for easily running a local Subspace blockchain.

## Dependencies

- rust (check out [`rust-toolchain.toml`](rust-toolchain.toml))
- protobuf compiler
- llvm

## Simplest example

Start a node and farmer and wait for 10 blocks being farmed.

```rust
use futures::prelude::*;

let node = subspace_sdk::Node::builder()
    .force_authoring(true)
    .role(subspace_sdk::node::Role::Authority)
    // Starting a new chain
    .build("node", subspace_sdk::chain_spec::dev_config().unwrap())
    .await
    .unwrap();

let plots = [subspace_sdk::PlotDescription::new("plot", bytesize::ByteSize::mb(100))];
let farmer = subspace_sdk::Farmer::builder()
    .build(subspace_sdk::PublicKey::from([0; 32]), node.clone(), &plots)
    .await
    .expect("Failed to init a farmer");

for plot in farmer.iter_plots().await {
    let mut plotting_progress = plot.subscribe_initial_plotting_progress().await;
    while plotting_progress.next().await.is_some() {}
}
tracing::info!("Initial plotting completed");

node.subscribe_new_blocks()
    .await
    .unwrap()
    // Wait 10 blocks and exit
    .take(10)
    .for_each(|block| async move { tracing::info!(?block, "New block!") })
    .await;
```

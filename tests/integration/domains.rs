use futures::prelude::*;

use crate::common::{Farmer, Node};

#[tokio::test(flavor = "multi_thread")]
async fn core_start() {
    crate::common::setup();

    let node = Node::dev().enable_core(true).build().await;
    let farmer = Farmer::dev().build(&node).await;

    node.system_domain()
        .unwrap()
        .payments()
        .unwrap()
        .subscribe_new_heads()
        .await
        .unwrap()
        .next()
        .await
        .unwrap();

    farmer.close().await;
    node.close().await;
}

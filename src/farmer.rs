use std::{io, net::SocketAddr, path::PathBuf};

use bytesize::ByteSize;
use either::Either;
use futures::Stream;
use libp2p_core::multiaddr::Multiaddr;
use tempdir::TempDir;

use crate::{Node, PublicKey};

// TODO: Should it be non-exhaustive?
pub struct PlotDescription {
    pub directory: Either<PathBuf, TempDir>,
    pub space_pledged: ByteSize,
}

impl PlotDescription {
    // TODO: should we check that plot is valid at this stage?
    // Or it can be literally a description of a plot
    pub fn new(directory: impl Into<PathBuf>, space_pledged: ByteSize) -> Self {
        Self {
            directory: Either::Left(directory.into()),
            space_pledged,
        }
    }

    pub fn with_tempdir(space_pledged: ByteSize) -> io::Result<Self> {
        TempDir::new("plot")
            .map(Either::Right)
            .map(|directory| Self {
                directory,
                space_pledged,
            })
    }
}

#[derive(Default)]
pub struct Builder {
    listen_on: Option<Multiaddr>,
    ws_rpc: Option<SocketAddr>,
}

impl Builder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn listen_on(mut self, multiaddr: Multiaddr) -> Self {
        self.listen_on = Some(multiaddr);
        self
    }

    pub fn ws_rpc(mut self, ws_rpc: SocketAddr) -> Self {
        self.ws_rpc = Some(ws_rpc);
        self
    }

    /// It supposed to open node at the supplied location
    // TODO: Should we just require multiple plots?
    pub async fn build(
        self,
        reward_address: PublicKey,
        node: Node,
        plot: PlotDescription,
    ) -> Result<Farmer, ()> {
        let _ = (reward_address, node, plot);
        todo!()
    }
}

pub struct Farmer {
    _ensure_cant_construct: (),
}

#[derive(Debug)]
#[non_exhaustive]
pub struct Info {
    pub version: String,
    pub reward_address: PublicKey,
    pub dsn_peers: u64,
    pub space_pledged: ByteSize,
}

#[derive(Debug)]
pub struct Solution {
    _ensure_cant_construct: (),
}

pub struct Plot {
    _ensure_cant_construct: (),
}

impl Plot {
    pub async fn delete(&mut self) {}
}

pub struct SolutionStream {
    _ensure_cant_construct: (),
}

impl Stream for SolutionStream {
    type Item = Solution;
    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        todo!()
    }
}

impl Farmer {
    pub fn builder() -> Builder {
        Builder::new()
    }

    pub async fn sync(&mut self) {}

    pub async fn start_farming(&mut self) {}

    pub async fn stop_farming(&mut self) {}

    pub async fn get_info(&mut self) -> Info {
        todo!()
    }

    pub async fn subscribe_solutions(&mut self) -> SolutionStream {
        todo!()
    }

    pub async fn plots(&mut self) -> &mut [Plot] {
        todo!()
    }

    // Stops farming, closes plots, and sends signal to the node
    pub async fn close(self) {}

    // Runs `.close()` and also wipes farmer's state
    pub async fn wipe(self) {}
}

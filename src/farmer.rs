use std::{io, path::PathBuf};

use bytesize::ByteSize;
use futures::{prelude::*, stream::FuturesUnordered};
use libp2p_core::Multiaddr;
use subspace_networking::{Node as DSNNode, NodeRunner as DSNNodeRunner};
use tokio::sync::oneshot;

use crate::{Node, PublicKey};

use subspace_farmer::single_disk_plot::{
    SingleDiskPlot, SingleDiskPlotError, SingleDiskPlotOptions,
};

// TODO: Should it be non-exhaustive?
pub struct PlotDescription {
    pub directory: PathBuf,
    pub space_pledged: ByteSize,
}

impl PlotDescription {
    // TODO: should we check that plot is valid at this stage?
    // Or it can be literally a description of a plot
    pub fn new(directory: impl Into<PathBuf>, space_pledged: ByteSize) -> Self {
        Self {
            directory: directory.into(),
            space_pledged,
        }
    }

    pub async fn wipe(self) -> io::Result<()> {
        tokio::fs::remove_dir_all(self.directory).await
    }
}

#[derive(Default)]
pub struct Builder {
    listen_on: Vec<Multiaddr>,
    bootstrap_nodes: Vec<Multiaddr>,
}

#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    #[error("Single disk plot creation error: {0}")]
    SingleDiskPlotCreate(#[from] SingleDiskPlotError),
    #[error("No plots error")]
    NoPlotsSupplied,
    #[error("Failed to connect to dsn: {0}")]
    DSNCreate(#[from] subspace_networking::CreationError),
}

async fn configure_dsn(
    listen_on: Vec<Multiaddr>,
    bootstrap_nodes: Vec<Multiaddr>,
) -> Result<(Option<DSNNode>, Option<DSNNodeRunner>), BuildError> {
    if bootstrap_nodes.is_empty() {
        return Ok((None, None));
    }

    let config =
        subspace_networking::Config {
            listen_on,
            allow_non_globals_in_dht: true,
            networking_parameters_registry:
                subspace_networking::BootstrappedNetworkingParameters::new(bootstrap_nodes).boxed(),
            request_response_protocols: vec![
                subspace_networking::PieceByHashRequestHandler::create(move |_req| {
                    // TODO: Implement actual handler
                    Some(subspace_networking::PieceByHashResponse { piece: None })
                }),
            ],
            ..subspace_networking::Config::with_generated_keypair()
        };

    subspace_networking::create(config)
        .await
        .map(|(node, node_runner)| (Some(node), Some(node_runner)))
        .map_err(Into::into)
}

impl Builder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn listen_on(mut self, multiaddrs: impl IntoIterator<Item = Multiaddr>) -> Self {
        self.listen_on = multiaddrs.into_iter().collect();
        self
    }

    pub fn bootstrap_nodes(mut self, multiaddrs: impl IntoIterator<Item = Multiaddr>) -> Self {
        self.bootstrap_nodes = multiaddrs.into_iter().collect();
        self
    }

    // pub fn ws_rpc(mut self, ws_rpc: SocketAddr) -> Self {
    //     self.ws_rpc = Some(ws_rpc);
    //     self
    // }

    /// It supposed to open node at the supplied location
    pub async fn build(
        self,
        reward_address: PublicKey,
        node: Node,
        plots: &[PlotDescription],
    ) -> Result<Farmer, BuildError> {
        if plots.is_empty() {
            return Err(BuildError::NoPlotsSupplied);
        }
        let Self {
            listen_on,
            bootstrap_nodes,
        } = self;

        let mut single_disk_plots = Vec::with_capacity(plots.len());
        let (dsn_node, dsn_node_runner) = configure_dsn(listen_on, bootstrap_nodes).await?;

        for description in plots {
            let description = SingleDiskPlotOptions {
                allocated_space: description.space_pledged.as_u64(),
                directory: description.directory.clone(),
                reward_address: *reward_address,
                rpc_client: node.clone(),
                dsn_node: dsn_node.clone(),
            };
            let single_disk_plot =
                tokio::task::spawn_blocking(move || SingleDiskPlot::new(description))
                    .await
                    .expect("Single disk plot never panics")?;
            single_disk_plots.push(single_disk_plot);
        }

        let mut single_disk_plots_stream = single_disk_plots
            .into_iter()
            .map(SingleDiskPlot::wait)
            .collect::<FuturesUnordered<_>>();

        let (cmd_sender, cmd_receiver) = oneshot::channel();

        if let Some(mut node_runner) = dsn_node_runner {
            tokio::spawn(async move {
                node_runner.run().await;
            });
        }

        let handle = tokio::spawn(async move {
            while let Some(result) = single_disk_plots_stream.next().await {
                result?;
            }
            anyhow::Ok(())
        });

        tokio::spawn(async move {
            let maybe_stop_sender = cmd_receiver.await;
            // TODO: remove once there won't be joining on drop in monorepo
            handle.abort();
            if let Ok(FarmerCommand::Stop(stop_sender)) = maybe_stop_sender {
                let _ = stop_sender.send(());
            }
        });

        Ok(Farmer { cmd_sender })
    }
}

#[derive(Debug)]
enum FarmerCommand {
    Stop(oneshot::Sender<()>),
}

pub struct Farmer {
    cmd_sender: oneshot::Sender<FarmerCommand>,
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
    pub async fn close(self) {
        let (stop_sender, stop_receiver) = oneshot::channel();
        if self
            .cmd_sender
            .send(FarmerCommand::Stop(stop_sender))
            .is_err()
        {
            return;
        }

        stop_receiver
            .await
            .expect("We should always receive here, as task is alive");
    }
}

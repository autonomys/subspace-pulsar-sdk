use std::{collections::HashMap, io, path::PathBuf};

use anyhow::Context;
use bytesize::ByteSize;
use futures::{prelude::*, stream::FuturesUnordered};
use libp2p_core::Multiaddr;
use subspace_core_primitives::SectorIndex;
use subspace_networking::{Node as DSNNode, NodeRunner as DSNNodeRunner};
use tokio::sync::oneshot;

use crate::{Node, PublicKey};

use subspace_farmer::single_disk_plot::{
    SingleDiskPlot, SingleDiskPlotError, SingleDiskPlotId, SingleDiskPlotInfo,
    SingleDiskPlotOptions, SingleDiskPlotSummary,
};

// TODO: Should it be non-exhaustive?
#[derive(Debug, Clone, PartialEq, Eq)]
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

        Ok(Farmer {
            cmd_sender,
            reward_address,
            plot_directories: plots.iter().map(|desc| desc.directory.clone()).collect(),
        })
    }
}

#[derive(Debug)]
enum FarmerCommand {
    Stop(oneshot::Sender<()>),
}

pub struct Farmer {
    cmd_sender: oneshot::Sender<FarmerCommand>,
    plot_directories: Vec<PathBuf>,
    reward_address: PublicKey,
}

#[derive(Debug)]
#[non_exhaustive]
// TODO: Should it be versioned?
pub struct PlotInfo {
    /// ID of the plot
    pub id: SingleDiskPlotId,
    /// Genesis hash of the chain used for plot creation
    pub genesis_hash: [u8; 32],
    /// Public key of identity used for plot creation
    pub public_key: PublicKey,
    /// First sector index in this plot
    ///
    /// Multiple plots can reuse the same identity, but they have to use different ranges for
    /// sector indexes or else they'll essentially plot the same data and will not result in
    /// increased probability of winning the reward.
    pub first_sector_index: SectorIndex,
    /// How much space in bytes is allocated for this plot
    pub allocated_space: ByteSize,
}

impl From<SingleDiskPlotInfo> for PlotInfo {
    fn from(info: SingleDiskPlotInfo) -> Self {
        let SingleDiskPlotInfo::V0 {
            id,
            genesis_hash,
            public_key,
            first_sector_index,
            allocated_space,
        } = info;
        Self {
            id,
            genesis_hash,
            public_key: super::PublicKey(public_key),
            first_sector_index,
            allocated_space: ByteSize::b(allocated_space),
        }
    }
}

#[derive(Debug)]
#[non_exhaustive]
pub struct Info {
    pub version: String,
    pub reward_address: PublicKey,
    // TODO: add dsn peers info
    // pub dsn_peers: u64,
    pub plots_info: HashMap<PathBuf, PlotInfo>,
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

    pub async fn get_info(&mut self) -> anyhow::Result<Info> {
        let plots_info = tokio::task::spawn_blocking({
            let dirs = self.plot_directories.clone();
            || {
                dirs.into_iter()
                    .map(SingleDiskPlot::collect_summary)
                    .collect::<Vec<_>>()
            }
        })
        .await?
        .into_iter()
        .map(|summary| match summary {
            SingleDiskPlotSummary::Found { info, directory } => Ok((directory, info.into())),
            SingleDiskPlotSummary::NotFound { directory } => {
                Err(anyhow::anyhow!("Didn't found plot at `{directory:?}'"))
            }
            SingleDiskPlotSummary::Error { directory, error } => {
                Err(error).context(format!("Failed to get plot summary at `{directory:?}'"))
            }
        })
        .collect::<anyhow::Result<_>>()?;

        Ok(Info {
            plots_info,
            version: env!("CARGO_PKG_VERSION").to_string(), // TODO: include git revision here
            reward_address: self.reward_address,
        })
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::{chain_spec, Node};
    use tempdir::TempDir;

    #[tokio::test(flavor = "multi_thread")]
    async fn test_get_info() {
        let dir = TempDir::new("test").unwrap();
        let node = Node::builder()
            .build(dir.path(), chain_spec::dev_config().unwrap())
            .await
            .unwrap();
        let plot_dir = TempDir::new("test").unwrap();
        let plots = [PlotDescription::new(
            plot_dir.as_ref(),
            bytesize::ByteSize::mb(10),
        )];
        let mut farmer = Farmer::builder()
            .build(Default::default(), node.clone(), &plots)
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;

        let Info {
            reward_address,
            plots_info,
            ..
        } = farmer.get_info().await.unwrap();
        assert_eq!(reward_address, Default::default());
        assert_eq!(plots_info.len(), 1);
        assert_eq!(
            plots_info[plot_dir.as_ref()].allocated_space,
            bytesize::ByteSize::mb(10)
        );

        farmer.close().await;
        node.close().await;
    }
}

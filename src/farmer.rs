use std::{collections::HashMap, io, path::PathBuf};

use anyhow::Context;
use bytesize::ByteSize;
use futures::{prelude::*, stream::FuturesUnordered};
use libp2p_core::Multiaddr;
use subspace_core_primitives::SectorIndex;
use subspace_networking::{Node as DSNNode, NodeRunner as DSNNodeRunner};
use subspace_rpc_primitives::SolutionResponse;
use tokio::sync::{oneshot, watch};

use crate::{Node, PublicKey};

use subspace_farmer::{
    single_disk_plot::{
        plotting::PlottedSector, SingleDiskPlot, SingleDiskPlotError, SingleDiskPlotId,
        SingleDiskPlotInfo, SingleDiskPlotOptions, SingleDiskPlotSummary,
    },
    RpcClient,
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
        let mut plot_info = HashMap::with_capacity(plots.len());
        let (dsn_node, dsn_node_runner) = configure_dsn(listen_on, bootstrap_nodes).await?;

        for description in plots {
            let directory = description.directory.clone();
            let allocated_space = description.space_pledged.as_u64();
            let description = SingleDiskPlotOptions {
                allocated_space,
                directory: directory.clone(),
                reward_address: *reward_address,
                rpc_client: node.clone(),
                dsn_node: dsn_node.clone(),
            };
            let single_disk_plot =
                tokio::task::spawn_blocking(move || SingleDiskPlot::new(description))
                    .await
                    .expect("Single disk plot never panics")?;
            let mut handlers = Vec::new();
            let progress = {
                let (sender, receiver) = watch::channel::<Option<_>>(None);
                let handler =
                    single_disk_plot.on_sector_plotted(std::sync::Arc::new(move |sector| {
                        let _ = sender.send(Some(sector.clone()));
                    }));
                handlers.push(handler);
                receiver
            };
            let solutions = {
                let (sender, receiver) = watch::channel::<Option<_>>(None);
                let handler = single_disk_plot.on_solution(std::sync::Arc::new(move |solution| {
                    let _ = sender.send(Some(solution.clone()));
                }));
                handlers.push(handler);
                receiver
            };
            let plot = Plot {
                directory: directory.clone(),
                allocated_space,
                progress,
                solutions,
                _handlers: handlers,
            };
            plot_info.insert(directory, plot);
            single_disk_plots.push(single_disk_plot);
        }

        let mut single_disk_plots_stream = single_disk_plots
            .into_iter()
            .map(SingleDiskPlot::run)
            .collect::<FuturesUnordered<_>>();

        if let Some(mut node_runner) = dsn_node_runner {
            tokio::spawn(async move {
                node_runner.run().await;
            });
        }

        let handle =
            tokio::spawn(async move { single_disk_plots_stream.next().await.unwrap().unwrap() });

        let (cmd_sender, cmd_receiver) = oneshot::channel();

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
            plot_info,
            node,
        })
    }
}

#[derive(Debug)]
enum FarmerCommand {
    Stop(oneshot::Sender<()>),
}

pub struct Farmer {
    cmd_sender: oneshot::Sender<FarmerCommand>,
    reward_address: PublicKey,
    plot_info: HashMap<PathBuf, Plot>,
    node: Node,
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
    // In bits
    pub sector_size: u64,
}

#[derive(Debug)]
pub struct Solution {
    _ensure_cant_construct: (),
}

pub struct Plot {
    directory: PathBuf,
    progress: watch::Receiver<Option<PlottedSector>>,
    solutions: watch::Receiver<Option<SolutionResponse>>,
    allocated_space: u64,
    _handlers: Vec<event_listener_primitives::HandlerId>,
}

impl Plot {
    pub fn directory(&self) -> &PathBuf {
        &self.directory
    }

    pub fn allocated_space(&self) -> ByteSize {
        ByteSize::b(self.allocated_space)
    }

    pub async fn subscribe_plotting_progress(
        &self,
    ) -> impl Stream<Item = PlottedSector> + Send + Sync + Unpin {
        tokio_stream::wrappers::WatchStream::new(self.progress.clone())
            .filter_map(futures::future::ready)
    }

    pub async fn subscribe_new_solutions(
        &self,
    ) -> impl Stream<Item = SolutionResponse> + Send + Sync + Unpin {
        tokio_stream::wrappers::WatchStream::new(self.solutions.clone())
            .filter_map(futures::future::ready)
    }
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

    pub async fn get_info(&mut self) -> anyhow::Result<Info> {
        let plots_info = tokio::task::spawn_blocking({
            let dirs = self.plot_info.keys().cloned().collect::<Vec<_>>();
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
            sector_size: self
                .node
                .farmer_protocol_info()
                .await
                .map(|info| subspace_core_primitives::plot_sector_size(info.space_l))
                .map_err(|err| anyhow::anyhow!("Failed to get farmer protocol info: {err}"))?,
        })
    }

    pub async fn iter_plots(&'_ mut self) -> impl Iterator<Item = &'_ Plot> + '_ {
        self.plot_info.values()
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

    #[tokio::test(flavor = "multi_thread")]
    async fn test_track_progress() {
        let dir = TempDir::new("test").unwrap();
        let node = Node::builder()
            .force_authoring(true)
            .role(sc_service::Role::Authority)
            .build(dir.path(), chain_spec::dev_config().unwrap())
            .await
            .unwrap();
        let plot_dir = TempDir::new("test").unwrap();
        let n_sectors = 1;
        let mut farmer = Farmer::builder()
            .build(
                Default::default(),
                node.clone(),
                &[PlotDescription::new(
                    plot_dir.as_ref(),
                    bytesize::ByteSize::mib(4 * n_sectors),
                )],
            )
            .await
            .unwrap();

        let progress = farmer
            .iter_plots()
            .await
            .next()
            .unwrap()
            .subscribe_plotting_progress()
            .await
            .take(n_sectors as usize)
            .collect::<Vec<_>>()
            .await;
        assert_eq!(progress.len(), n_sectors as usize);

        farmer.close().await;
        node.close().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_new_solution() {
        let dir = TempDir::new("test").unwrap();
        let node = Node::builder()
            .force_authoring(true)
            .role(sc_service::Role::Authority)
            .build(dir.path(), chain_spec::dev_config().unwrap())
            .await
            .unwrap();
        let plot_dir = TempDir::new("test").unwrap();
        let mut farmer = Farmer::builder()
            .build(
                Default::default(),
                node.clone(),
                &[PlotDescription::new(
                    plot_dir.as_ref(),
                    bytesize::ByteSize::mib(4),
                )],
            )
            .await
            .unwrap();

        farmer
            .iter_plots()
            .await
            .next()
            .unwrap()
            .subscribe_new_solutions()
            .await
            .next()
            .await
            .expect("Farmer should send new solutions");

        farmer.close().await;
        node.close().await;
    }
}

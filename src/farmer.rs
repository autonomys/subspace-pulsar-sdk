use std::{io, path::PathBuf};

use bytesize::ByteSize;
use either::*;
use futures::{prelude::*, stream::FuturesUnordered};
use tempdir::TempDir;

use crate::{Node, PublicKey};

use subspace_farmer::single_disk_plot::{
    SingleDiskPlot, SingleDiskPlotError, SingleDiskPlotOptions,
};

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
    _ensure_cant_construct: (),
    // TODO: add once those will be used
    // listen_on: Option<Multiaddr>,
    // ws_rpc: Option<SocketAddr>,
}

#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    #[error("Single disk plot creation error: {0}")]
    SingleDiskPlotCreate(#[from] SingleDiskPlotError),
    #[error("No plots error")]
    NoPlotsSupplied,
}

impl Builder {
    pub fn new() -> Self {
        Self::default()
    }

    // TODO: add once those will be used
    // pub fn listen_on(mut self, multiaddr: Multiaddr) -> Self {
    //     self.listen_on = Some(multiaddr);
    //     self
    // }
    //
    // pub fn ws_rpc(mut self, ws_rpc: SocketAddr) -> Self {
    //     self.ws_rpc = Some(ws_rpc);
    //     self
    // }

    // TODO: put some proper value
    const CMD_CHANNEL_SIZE: usize = 10;

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

        let mut single_disk_plots = Vec::with_capacity(plots.len());

        for description in plots {
            let description = SingleDiskPlotOptions {
                allocated_space: description.space_pledged.as_u64(),
                directory: description
                    .directory
                    .as_ref()
                    .map_left(AsRef::as_ref)
                    .left_or_else(|tempdir| tempdir.path())
                    .to_owned(),
                reward_address,
                rpc_client: node.clone(),
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

        let (cmd_sender, mut cmd_receiver) =
            futures::channel::mpsc::channel(Self::CMD_CHANNEL_SIZE);
        tokio::spawn(async move {
            let maybe_stop_sender = loop {
                let cmd = match futures::future::select(
                    single_disk_plots_stream.next(),
                    cmd_receiver.next(),
                )
                .await
                {
                    futures::future::Either::Left((Some(result), _)) => {
                        result?;
                        continue;
                    }
                    futures::future::Either::Left((None, _)) => return anyhow::Ok(()),
                    futures::future::Either::Right((None, _)) => break None,
                    futures::future::Either::Right((Some(cmd), _)) => cmd,
                };

                match cmd {
                    FarmerCommand::Stop(stop_sender) => {
                        break Some(stop_sender);
                    }
                }
            };

            drop(single_disk_plots_stream);
            if let Some(stop_sender) = maybe_stop_sender {
                let _ = stop_sender.send(());
            }
            Ok(())
        });

        Ok(Farmer { cmd_sender })
    }
}

enum FarmerCommand {
    Stop(futures::channel::oneshot::Sender<()>),
}

pub struct Farmer {
    cmd_sender: futures::channel::mpsc::Sender<FarmerCommand>,
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
    pub async fn close(mut self) -> anyhow::Result<()> {
        let (stop_sender, stop_receiver) = futures::channel::oneshot::channel();
        if self
            .cmd_sender
            .send(FarmerCommand::Stop(stop_sender))
            .await
            .is_err()
        {
            return Err(anyhow::anyhow!("Noone is listening for our commands"));
        }

        stop_receiver
            .await
            .expect("We should always receive here, as task is alive");
        Ok(())
    }

    // Runs `.close()` and also wipes farmer's state
    pub async fn wipe(self) -> anyhow::Result<()> {
        self.close().await?;
        // TODO: add actual wiping of data
        Ok(())
    }
}

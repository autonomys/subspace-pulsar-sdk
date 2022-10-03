use std::path::PathBuf;

use crate::Directory;

#[non_exhaustive]
#[derive(Debug, Default)]
pub enum Mode {
    #[default]
    Full,
}

#[non_exhaustive]
#[derive(Debug, Default)]
pub enum Network {
    #[default]
    Gemini2a,
    // TODO: put proper type here
    Custom(std::convert::Infallible),
}

#[derive(Default)]
pub struct Builder {
    mode: Mode,
    network: Network,
    name: Option<String>,
    directory: Directory,
    port: u16,
}

impl Builder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn mode(mut self, ty: Mode) -> Self {
        self.mode = ty;
        self
    }

    pub fn network(mut self, network: Network) -> Self {
        self.network = network;
        self
    }

    pub fn name(mut self, name: impl AsRef<str>) -> Self {
        if !name.as_ref().is_empty() {
            self.name = Some(name.as_ref().to_owned());
        }
        self
    }

    pub fn directory(mut self, path: impl Into<PathBuf>) -> Self {
        self.directory = Directory::Custom(path.into());
        self
    }

    pub fn tmp_directory(mut self) -> Self {
        self.directory = Directory::Tmp;
        self
    }

    pub fn port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }

    /// It supposed to open node at the supplied location
    pub async fn build(self) -> Result<Node, ()> {
        todo!()
    }
}

#[derive(Clone)]
pub struct Node {
    _ensure_cant_construct: (),
}

#[derive(Debug)]
#[non_exhaustive]
pub struct Info {
    pub version: String,
    pub network: Network,
    pub mode: Mode,
    pub name: Option<String>,
    pub connected_peers: u64,
    pub best_block: u64,
    pub total_space_pledged: u64,
    pub total_history_size: u64,
    pub space_pledged: u64,
}

#[derive(Debug)]
pub struct Block {
    _ensure_cant_construct: (),
}

impl Node {
    pub fn builder() -> Builder {
        Builder::new()
    }

    pub async fn sync(&mut self) {}

    // Leaves the network and gracefully shuts down
    pub async fn close(self) {}

    // Runs `.close()` and also wipes node's state
    pub async fn wipe(self) {}

    pub async fn get_info(&mut self) -> Info {
        todo!()
    }

    pub async fn on_block<H, F>(&mut self, callback: H)
    where
        H: Clone + Send + Sync + 'static + FnMut(Block) -> F,
        F: Send + 'static + std::future::Future<Output = ()>,
    {
        let _ = callback;
    }
}

use futures::Stream;

use crate::Directory;

#[non_exhaustive]
#[derive(Debug, Default)]
pub enum Mode {
    #[default]
    Full,
}

#[non_exhaustive]
#[derive(Debug, Default)]
pub enum Chain {
    #[default]
    Gemini2a,
    // TODO: put proper type here
    Custom(std::convert::Infallible),
}

#[derive(Default)]
pub struct Builder {
    mode: Mode,
    chain: Chain,
    directory: Directory,
    name: Option<String>,
    port: u16,
    validate: bool,
}

impl Builder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn mode(mut self, ty: Mode) -> Self {
        self.mode = ty;
        self
    }

    pub fn chain(mut self, chain: Chain) -> Self {
        self.chain = chain;
        self
    }

    pub fn validator(mut self, validate: bool) -> Self {
        self.validate = validate;
        self
    }

    pub fn name(mut self, name: impl AsRef<str>) -> Self {
        if !name.as_ref().is_empty() {
            self.name = Some(name.as_ref().to_owned());
        }
        self
    }

    pub fn port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }

    pub fn at_directory(mut self, directory: impl Into<Directory>) -> Self {
        self.directory = directory.into();
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
    pub chain: Chain,
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

pub struct BlockStream {
    _ensure_cant_construct: (),
}

impl Stream for BlockStream {
    type Item = Block;
    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        todo!()
    }
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

    pub async fn subscribe_new_blocks(&mut self) -> BlockStream {
        todo!()
    }
}

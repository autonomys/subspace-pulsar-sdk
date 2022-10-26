use futures::Stream;
use std::sync::Arc;

use cirrus_runtime::GenesisConfig as ExecutionGenesisConfig;
use futures::StreamExt;
use sc_cli::{ChainSpec, CliConfiguration, SubstrateCli};
use sc_consensus_slots::SlotProportion;
use sc_subspace_chain_specs::ExecutionChainSpec;
use sp_core::crypto::Ss58AddressFormat;
use subspace_node::{ExecutorDispatch, SecondaryChainCli};
use subspace_runtime::RuntimeApi;
use subspace_service::SubspaceConfiguration;

use crate::{
    utils::{self, AbortingJoinHandle},
    Directory,
};

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

struct Port(u16);

impl Default for Port {
    fn default() -> Self {
        Self(30_333)
    }
}

#[derive(Default)]
pub struct Builder {
    mode: Mode,
    chain: Chain,
    directory: Directory,
    name: Option<String>,
    port: Port,
    validate: bool,
}

fn set_default_ss58_version<C: AsRef<dyn ChainSpec>>(chain_spec: C) {
    let maybe_ss58_address_format = chain_spec
        .as_ref()
        .properties()
        .get("ss58Format")
        .map(|v| {
            v.as_u64()
                .expect("ss58Format must always be an unsigned number; qed")
        })
        .map(|v| {
            v.try_into()
                .expect("ss58Format must always be within u16 range; qed")
        })
        .map(Ss58AddressFormat::custom);

    if let Some(ss58_address_format) = maybe_ss58_address_format {
        sp_core::crypto::set_default_ss58_version(ss58_address_format);
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    #[error("Failed to create the runner for node: {0}")]
    CreateRunner(#[source] sc_cli::Error),
    #[error("Failed to get base path: {0}")]
    NoBasePath(#[source] sc_cli::Error),
    #[error("Failed to create secondary chain configuration: {0}")]
    CreateSecondaryChainSpec(#[source] sc_cli::Error),
    #[error("Failed to convert network keypair: {0:?}")]
    ConvertNetworkKeyPair(#[source] std::io::Error),
    #[error("Failed to build a full subspace node: {0:?}")]
    BuildNode(#[source] subspace_service::Error),
    #[error("Failed to build a secondary subspace node: {0:?}")]
    BuildSecondaryChainNode(#[source] sc_cli::Error),
    #[error("Primary chain spec must contain secondary chain spec")]
    PrimaryChainSpecMustContainSecondary,
    #[error("Other: {0:?}")]
    Other(#[from] sc_service::Error),
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
        self.port = Port(port);
        self
    }

    pub fn at_directory(mut self, directory: impl Into<Directory>) -> Self {
        self.directory = directory.into();
        self
    }

    /// It supposed to open node at the supplied location
    pub async fn build(self) -> Result<Node, BuildError> {
        struct SecondaryExecutorDispatch;

        impl sc_executor::NativeExecutionDispatch for SecondaryExecutorDispatch {
            type ExtendHostFunctions = ();

            fn dispatch(method: &str, data: &[u8]) -> Option<Vec<u8>> {
                cirrus_runtime::api::dispatch(method, data)
            }

            fn native_version() -> sc_executor::NativeVersion {
                cirrus_runtime::native_version()
            }
        }

        #[derive(clap::Parser)]
        #[clap(version = "0.1-some-version-so-clippy-won't-panic")]
        struct Cli {
            #[clap(flatten)]
            node: subspace_node::Cli,
        }

        let Self {
            mode: Mode::Full,
            chain,
            directory,
            name,
            port: Port(port),
            validate,
        } = self;
        let mut args = vec![std::ffi::OsString::from("node-binary-place-holder")];

        args.extend_from_slice(&["--state-pruning".into(), "archive".into()]);
        match chain {
            Chain::Gemini2a => args.extend_from_slice(&["--chain".into(), "gemini-2a".into()]),
            Chain::Custom(chain) => match chain {},
        };
        match &directory {
            Directory::Tmp => args.extend_from_slice(&["--tmp".into()]),
            Directory::Custom(path) => args.extend_from_slice(&["--base-path".into(), path.into()]),
            Directory::Default => (), // TODO: add default path here
        };
        if let Some(name) = name {
            args.extend_from_slice(&["--name".into(), name.into()]);
        }
        if validate {
            args.extend_from_slice(&["--validate".into()]);
        }
        args.extend_from_slice(&["--port".into(), port.to_string().into()]);

        let Cli { node: mut cli } = <Cli as clap::Parser>::parse_from(args);

        // Increase default number of peers
        cli.run.network_params.out_peers = 50;

        let handle = tokio::task::spawn_blocking(move || {
            let runner = cli
                .create_runner(&cli.run)
                .map_err(BuildError::CreateRunner)?;
            set_default_ss58_version(&runner.config().chain_spec);
            runner.run_node_until_exit(|primary_chain_config| async move {
                let tokio_handle = primary_chain_config.tokio_handle.clone();

                let maybe_secondary_chain_spec = primary_chain_config
                    .chain_spec
                    .extensions()
                    .get_any(std::any::TypeId::of::<
                        ExecutionChainSpec<ExecutionGenesisConfig>,
                    >())
                    .downcast_ref()
                    .cloned();

                // TODO: proper value
                let block_import_throttling_buffer_size = 10;

                let mut primary_chain_node = {
                    let span = sc_tracing::tracing::info_span!(
                        sc_tracing::logging::PREFIX_LOG_SPAN,
                        name = "PrimaryChain"
                    );
                    let _enter = span.enter();

                    let dsn_config = {
                        let network_keypair = primary_chain_config
                            .network
                            .node_key
                            .clone()
                            .into_keypair()
                            .map_err(BuildError::ConvertNetworkKeyPair)?;

                        (!cli.dsn_listen_on.is_empty()).then(|| subspace_networking::Config {
                            keypair: network_keypair,
                            listen_on: cli.dsn_listen_on,
                            ..subspace_networking::Config::with_generated_keypair()
                        })
                    };

                    let primary_chain_config = SubspaceConfiguration {
                        base: primary_chain_config,
                        // Secondary node needs slots notifications for bundle production.
                        force_new_slot_notifications: !cli.secondary_chain_args.is_empty(),
                        dsn_config,
                    };

                    subspace_service::new_full::<RuntimeApi, ExecutorDispatch>(
                        primary_chain_config,
                        true,
                        SlotProportion::new(2f32 / 3f32),
                    )
                    .await
                    .map_err(BuildError::BuildNode)?
                };

                // Run an executor node, an optional component of Subspace full node.
                if !cli.secondary_chain_args.is_empty() {
                    let span = sc_tracing::tracing::info_span!(
                        sc_tracing::logging::PREFIX_LOG_SPAN,
                        name = "SecondaryChain"
                    );
                    let _enter = span.enter();

                    let mut secondary_chain_cli = SecondaryChainCli::new(
                        cli.run
                            .base_path()
                            .map_err(BuildError::NoBasePath)?
                            .map(|base_path| base_path.path().to_path_buf()),
                        maybe_secondary_chain_spec
                            .ok_or(BuildError::PrimaryChainSpecMustContainSecondary)?,
                        cli.secondary_chain_args.iter(),
                    );

                    // Increase default number of peers
                    if secondary_chain_cli.run.network_params.out_peers == 25 {
                        secondary_chain_cli.run.network_params.out_peers = 50;
                    }

                    let secondary_chain_config = SubstrateCli::create_configuration(
                        &secondary_chain_cli,
                        &secondary_chain_cli,
                        tokio_handle,
                    )
                    .map_err(BuildError::CreateSecondaryChainSpec)?;

                    let secondary_chain_node = cirrus_node::service::new_full::<
                        _,
                        _,
                        _,
                        _,
                        _,
                        cirrus_runtime::RuntimeApi,
                        SecondaryExecutorDispatch,
                    >(
                        secondary_chain_config,
                        primary_chain_node.client.clone(),
                        primary_chain_node.network.clone(),
                        &primary_chain_node.select_chain,
                        primary_chain_node
                            .imported_block_notification_stream
                            .subscribe()
                            .then(|imported_block_notification| async move {
                                (
                                    imported_block_notification.block_number,
                                    imported_block_notification.fork_choice,
                                    imported_block_notification.block_import_acknowledgement_sender,
                                )
                            }),
                        primary_chain_node
                            .new_slot_notification_stream
                            .subscribe()
                            .then(|slot_notification| async move {
                                (
                                    slot_notification.new_slot_info.slot,
                                    slot_notification.new_slot_info.global_challenge,
                                )
                            }),
                        block_import_throttling_buffer_size,
                    )
                    .await?;

                    primary_chain_node
                        .task_manager
                        .add_child(secondary_chain_node.task_manager);

                    secondary_chain_node.network_starter.start_network();
                }

                primary_chain_node.network_starter.start_network();
                Ok::<_, BuildError>(primary_chain_node.task_manager)
            })?;

            Ok(())
        });

        Ok(Node {
            _handle: Arc::new(AbortingJoinHandle::new(handle)),
        })
    }
}

#[derive(Clone)]
pub struct Node {
    _handle: Arc<utils::AbortingJoinHandle<Result<(), BuildError>>>,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_start_node() {
        Node::builder().build().await.unwrap();
    }
}

//! Implementation of command line option for running server

use std::sync::Arc;

use crate::{
    influxdb_ioxd::{
        self,
        server_type::{
            common_state::{CommonServerState, CommonServerStateError},
            test::{TestAction, TestServerType},
        },
    },
    structopt_blocks::run_config::RunConfig,
};
use metric::Registry;
use structopt::StructOpt;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("Run: {0}")]
    Run(#[from] influxdb_ioxd::Error),

    #[error("Invalid config: {0}")]
    InvalidConfig(#[from] CommonServerStateError),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, StructOpt)]
#[structopt(
    name = "run",
    about = "Runs in test mode",
    long_about = "Run the IOx test server.\n\nThe configuration options below can be \
    set either with the command line flags or with the specified environment \
    variable. If there is a file named '.env' in the current working directory, \
    it is sourced before loading the configuration.

Configuration is loaded from the following sources (highest precedence first):
        - command line arguments
        - user set environment variables
        - .env file contents
        - pre-configured default values"
)]
pub struct Config {
    #[structopt(flatten)]
    pub(crate) run_config: RunConfig,

    /// Test action
    #[structopt(
        long = "--test-action",
        env = "IOX_TEST_ACTION",
        default_value = "None",
        possible_values = &TestAction::variants(),
        case_insensitive = true,
    )]
    test_action: TestAction,
}

pub async fn command(config: Config) -> Result<()> {
    let common_state = CommonServerState::from_config(config.run_config.clone())?;
    let server_type = Arc::new(TestServerType::new(
        Arc::new(Registry::new()),
        common_state.trace_collector(),
        config.test_action,
    ));

    Ok(influxdb_ioxd::main(common_state, server_type).await?)
}
use snafu::{ResultExt, Snafu};

use clap_blocks::run_config::RunConfig;

mod all_in_one;
mod compactor;
mod database;
mod ingester;
mod querier;
mod router;
mod router2;
mod test;

#[derive(Debug, Snafu)]
#[allow(clippy::enum_variant_names)]
pub enum Error {
    #[snafu(display("Error in compactor subcommand: {}", source))]
    CompactorError { source: compactor::Error },

    #[snafu(display("Error in database subcommand: {}", source))]
    DatabaseError { source: database::Error },

    #[snafu(display("Error in querier subcommand: {}", source))]
    QuerierError { source: querier::Error },

    #[snafu(display("Error in router subcommand: {}", source))]
    RouterError { source: router::Error },

    #[snafu(display("Error in router2 subcommand: {}", source))]
    Router2Error { source: router2::Error },

    #[snafu(display("Error in ingester subcommand: {}", source))]
    IngesterError { source: ingester::Error },

    #[snafu(display("Error in all in one subcommand: {}", source))]
    AllInOneError { source: all_in_one::Error },

    #[snafu(display("Error in test subcommand: {}", source))]
    TestError { source: test::Error },
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, clap::Parser)]
pub struct Config {
    // TODO(marco) remove this
    /// Config for database mode, for backwards compatibility reasons.
    #[clap(flatten)]
    database_config: database::Config,

    #[clap(subcommand)]
    command: Option<Command>,
}

impl Config {
    pub fn run_config(&self) -> &RunConfig {
        match &self.command {
            None => &self.database_config.run_config,
            Some(Command::Compactor(config)) => &config.run_config,
            Some(Command::Database(config)) => &config.run_config,
            Some(Command::Querier(config)) => &config.run_config,
            Some(Command::Router(config)) => &config.run_config,
            Some(Command::Router2(config)) => &config.run_config,
            Some(Command::Ingester(config)) => &config.run_config,
            Some(Command::AllInOne(config)) => &config.run_config,
            Some(Command::Test(config)) => &config.run_config,
        }
    }
}

#[derive(Debug, clap::Parser)]
enum Command {
    /// Run the server in compactor mode
    Compactor(compactor::Config),

    /// Run the server in database mode (Deprecated)
    Database(database::Config),

    /// Run the server in querier mode
    Querier(querier::Config),

    /// Run the server in routing mode (Deprecated)
    Router(router::Config),

    /// Run the server in router2 mode
    Router2(router2::Config),

    /// Run the server in ingester mode
    Ingester(ingester::Config),

    /// Run the server in "all in one" mode
    AllInOne(all_in_one::Config),

    /// Run the server in test mode
    Test(test::Config),
}

pub async fn command(config: Config) -> Result<()> {
    match config.command {
        None => {
            println!(
                "WARNING: Not specifying the run-mode is deprecated. Defaulting to 'database'."
            );
            database::command(config.database_config)
                .await
                .context(DatabaseSnafu)
        }
        Some(Command::Compactor(config)) => {
            compactor::command(config).await.context(CompactorSnafu)
        }
        Some(Command::Database(config)) => database::command(config).await.context(DatabaseSnafu),
        Some(Command::Querier(config)) => querier::command(config).await.context(QuerierSnafu),
        Some(Command::Router(config)) => router::command(config).await.context(RouterSnafu),
        Some(Command::Router2(config)) => router2::command(config).await.context(Router2Snafu),
        Some(Command::Ingester(config)) => ingester::command(config).await.context(IngesterSnafu),
        Some(Command::AllInOne(config)) => all_in_one::command(config).await.context(AllInOneSnafu),
        Some(Command::Test(config)) => test::command(config).await.context(TestSnafu),
    }
}

//! Entrypoint of InfluxDB IOx binary
#![deny(rustdoc::broken_intra_doc_links, rustdoc::bare_urls, rust_2018_idioms)]
#![warn(
    missing_debug_implementations,
    clippy::explicit_iter_loop,
    clippy::use_self,
    clippy::clone_on_ref_ptr,
    clippy::future_not_send
)]

use influxdb_iox::{
    commands::{self, tracing::{init_logs_and_tracing, init_simple_logs, TroggingGuard}},
    ReturnCode, KeyValue, VERSION_STRING, install_crash_handler, load_dotenv, get_runtime,
};
use influxdb_iox_client::connection::Builder;
use observability_deps::tracing::warn;

#[derive(Debug, clap::Parser)]
#[clap(
    name = "influxdb_iox",
    version = &VERSION_STRING[..],
    about = "InfluxDB IOx server and command line tools",
    long_about = r#"InfluxDB IOx server and command line tools

Examples:
    # Run the InfluxDB IOx server:
    influxdb_iox run database

    # Run the interactive SQL prompt
    influxdb_iox sql

    # Display all server settings
    influxdb_iox run database --help

    # Run the InfluxDB IOx server with extra verbose logging
    influxdb_iox run database -v

    # Run InfluxDB IOx with full debug logging specified with LOG_FILTER
    LOG_FILTER=debug influxdb_iox run database

Command are generally structured in the form:
    <type of object> <action> <arguments>

For example, a command such as the following shows all actions
    available for database chunks, including get and list.

    influxdb_iox database chunk --help
"#
)]
struct Config {
    /// Log filter short-hand.
    ///
    /// Convenient way to set log severity level filter.
    /// Overrides --log-filter / LOG_FILTER.
    ///
    /// -v   'info'
    ///
    /// -vv  'debug,hyper::proto::h1=info,h2=info'
    ///
    /// -vvv 'trace,hyper::proto::h1=info,h2=info'
    #[clap(
        short = 'v',
        long = "--verbose",
        multiple_occurrences = true,
        takes_value = false,
        parse(from_occurrences)
    )]
    pub log_verbose_count: u8,

    /// gRPC address of IOx server to connect to
    #[clap(
        short,
        long,
        global = true,
        env = "IOX_ADDR",
        default_value = "http://127.0.0.1:8082"
    )]
    host: String,

    /// Additional headers to add to CLI requests
    ///
    /// Values should be key value pairs separated by ':'
    #[clap(long, global = true)]
    header: Vec<KeyValue<http::header::HeaderName, http::HeaderValue>>,

    #[clap(long)]
    /// Set the maximum number of threads to use. Defaults to the number of
    /// cores on the system
    num_threads: Option<usize>,

    #[clap(subcommand)]
    command: Command,
}

#[derive(Debug, clap::Parser)]
enum Command {
    /// Database-related commands
    Database(commands::database::Config),

    /// Run the InfluxDB IOx server
    // Clippy recommended boxing this variant because it's much larger than the others
    Run(Box<commands::run::Config>),

    /// Router-related commands
    Router(commands::router::Config),

    /// IOx server configuration commands
    Server(commands::server::Config),

    /// Manage long-running IOx operations
    Operation(commands::operations::Config),

    /// Start IOx interactive SQL REPL loop
    Sql(commands::sql::Config),

    /// Various commands for catalog manipulation
    Catalog(commands::catalog::Config),

    /// Interrogate internal database data
    Debug(commands::debug::Config),

    /// Initiate a read request to the gRPC storage service.
    Storage(commands::storage::Config),
}

fn main() -> Result<(), std::io::Error> {
    install_crash_handler(); // attempt to render a useful stacktrace to stderr

    // load all environment variables from .env before doing anything
    load_dotenv();

    let config: Config = clap::Parser::parse();

    let tokio_runtime = get_runtime(config.num_threads)?;
    tokio_runtime.block_on(async move {
        let host = config.host;
        let headers = config.header;
        let log_verbose_count = config.log_verbose_count;

        let connection = || async move {
            let builder = headers.into_iter().fold(Builder::default(), |builder, kv| {
                builder.header(kv.key, kv.value)
            });

            match builder.build(&host).await {
                Ok(connection) => connection,
                Err(e) => {
                    eprintln!("Error connecting to {}: {}", host, e);
                    std::process::exit(ReturnCode::Failure as _)
                }
            }
        };

        fn handle_init_logs(r: Result<TroggingGuard, trogging::Error>) -> TroggingGuard {
            match r {
                Ok(guard) => guard,
                Err(e) => {
                    eprintln!("Initializing logs failed: {}", e);
                    std::process::exit(ReturnCode::Failure as _);
                }
            }
        }

        match config.command {
            Command::Database(config) => {
                let _tracing_guard = handle_init_logs(init_simple_logs(log_verbose_count));
                let connection = connection().await;
                if let Err(e) = commands::database::command(connection, config).await {
                    eprintln!("{}", e);
                    std::process::exit(ReturnCode::Failure as _)
                }
            }
            Command::Operation(config) => {
                let _tracing_guard = handle_init_logs(init_simple_logs(log_verbose_count));
                let connection = connection().await;
                if let Err(e) = commands::operations::command(connection, config).await {
                    eprintln!("{}", e);
                    std::process::exit(ReturnCode::Failure as _)
                }
            }
            Command::Server(config) => {
                let _tracing_guard = handle_init_logs(init_simple_logs(log_verbose_count));
                let connection = connection().await;
                if let Err(e) = commands::server::command(connection, config).await {
                    eprintln!("Server command failed: {}", e);
                    std::process::exit(ReturnCode::Failure as _)
                }
            }
            Command::Router(config) => {
                let _tracing_guard = handle_init_logs(init_simple_logs(log_verbose_count));
                let connection = connection().await;
                if let Err(e) = commands::router::command(connection, config).await {
                    eprintln!("{}", e);
                    std::process::exit(ReturnCode::Failure as _)
                }
            }
            Command::Run(config) => {
                let _tracing_guard =
                    handle_init_logs(init_logs_and_tracing(log_verbose_count, &config));
                if let Err(e) = commands::run::command(*config).await {
                    eprintln!("Server command failed: {}", e);
                    std::process::exit(ReturnCode::Failure as _)
                }
            }
            Command::Sql(config) => {
                let _tracing_guard = handle_init_logs(init_simple_logs(log_verbose_count));
                let connection = connection().await;
                if let Err(e) = commands::sql::command(connection, config).await {
                    eprintln!("{}", e);
                    std::process::exit(ReturnCode::Failure as _)
                }
            }
            Command::Storage(config) => {
                let _tracing_guard = handle_init_logs(init_simple_logs(log_verbose_count));
                let connection = connection().await;
                if let Err(e) = commands::storage::command(connection, config).await {
                    eprintln!("{}", e);
                    std::process::exit(ReturnCode::Failure as _)
                }
            }
            Command::Catalog(config) => {
                let _tracing_guard = handle_init_logs(init_simple_logs(log_verbose_count));
                if let Err(e) = commands::catalog::command(config).await {
                    eprintln!("{}", e);
                    std::process::exit(ReturnCode::Failure as _)
                }
            }
            Command::Debug(config) => {
                let _tracing_guard = handle_init_logs(init_simple_logs(log_verbose_count));
                if let Err(e) = commands::debug::command(config).await {
                    eprintln!("{}", e);
                    std::process::exit(ReturnCode::Failure as _)
                }
            }
        }
    });

    Ok(())
}

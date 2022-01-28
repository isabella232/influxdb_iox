use trace_exporters::TracingConfig;
use trogging::cli::LoggingConfig;

use crate::{object_store::ObjectStoreConfig, server_id::ServerIdConfig, socket_addr::SocketAddr};

#[derive(Debug, Clone)]
pub enum ServingReadinessState {
    Unavailable,
    Serving,
}

impl std::str::FromStr for ServingReadinessState {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "unavailable" => Ok(Self::Unavailable),
            "serving" => Ok(Self::Serving),
            _ => Err(format!(
                "Invalid serving readiness format '{}'. Valid options: unavailable, serving",
                s
            )),
        }
    }
}

impl From<bool> for ServingReadinessState {
    fn from(v: bool) -> Self {
        match v {
            true => Self::Serving,
            false => Self::Unavailable,
        }
    }
}

impl From<ServingReadinessState> for bool {
    fn from(state: ServingReadinessState) -> Self {
        match state {
            ServingReadinessState::Unavailable => false,
            ServingReadinessState::Serving => true,
        }
    }
}

/// The default bind address for the HTTP API.
pub const DEFAULT_API_BIND_ADDR: &str = "127.0.0.1:8080";

/// The default bind address for the gRPC.
pub const DEFAULT_GRPC_BIND_ADDR: &str = "127.0.0.1:8082";

/// Common config for all `run` commands.
#[derive(Debug, Clone, clap::Parser)]
pub struct RunConfig {
    /// logging options
    #[clap(flatten)]
    pub logging_config: LoggingConfig,

    /// tracing options
    #[clap(flatten)]
    pub tracing_config: TracingConfig,

    /// server ID config
    #[clap(flatten)]
    pub server_id_config: ServerIdConfig,

    /// The address on which IOx will serve HTTP API requests.
    #[clap(
    long = "--api-bind",
    env = "INFLUXDB_IOX_BIND_ADDR",
    default_value = DEFAULT_API_BIND_ADDR,
    )]
    pub http_bind_address: SocketAddr,

    /// The address on which IOx will serve Storage gRPC API requests.
    #[clap(
    long = "--grpc-bind",
    env = "INFLUXDB_IOX_GRPC_BIND_ADDR",
    default_value = DEFAULT_GRPC_BIND_ADDR,
    )]
    pub grpc_bind_address: SocketAddr,

    /// After startup the IOx server can either accept serving data plane traffic right away
    /// or require a SetServingReadiness call from the Management API to enable serving.
    #[clap(
        long = "--initial-serving-readiness-state",
        env = "INFLUXDB_IOX_INITIAL_SERVING_READINESS_STATE",
        default_value = "serving"
    )]
    pub initial_serving_state: ServingReadinessState,

    /// Maximum size of HTTP requests.
    #[clap(
        long = "--max-http-request-size",
        env = "INFLUXDB_IOX_MAX_HTTP_REQUEST_SIZE",
        default_value = "10485760" // 10 MiB
    )]
    pub max_http_request_size: usize,

    /// object store config
    #[clap(flatten)]
    pub object_store_config: ObjectStoreConfig,
}

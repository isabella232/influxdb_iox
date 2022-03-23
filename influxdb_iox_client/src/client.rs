/// Errors for the client
pub mod error;

/// Client for health checking API
pub mod health;

/// Client for delete API
pub mod delete;

/// Client for deployment API
pub mod deployment;

/// Client for management API
pub mod management;

/// Client for remote API
pub mod remote;

/// Client for router API
pub mod router;

/// Client for schema API
pub mod schema;

/// Client for write API
pub mod write;

/// Client for long running operations API
pub mod operations;

#[cfg(feature = "flight")]
/// Client for query API (based on Arrow flight)
pub mod flight;

//! Client helpers for writing end to end ng tests
use std::time::Duration;

use arrow::record_batch::RecordBatch;
use http::Response;
use hyper::{Body, Client, Request};

use influxdb_iox_client::connection::Connection;

/// Writes the line protocol to the write_base/api/v2/write endpoint (typically on the router)
pub async fn write_to_router(
    line_protocol: impl Into<String>,
    org: impl AsRef<str>,
    bucket: impl AsRef<str>,
    write_base: impl AsRef<str>,
) -> Response<Body> {
    let client = Client::new();
    let url = format!(
        "{}/api/v2/write?org={}&bucket={}",
        write_base.as_ref(),
        org.as_ref(),
        bucket.as_ref()
    );

    let request = Request::builder()
        .uri(url)
        .method("POST")
        .body(Body::from(line_protocol.into()))
        .expect("failed to construct HTTP request");

    client
        .request(request)
        .await
        .expect("http error sending write")
}

/// Runs a query using the flight API on the specified connection
/// until responses are produced
///
/// The retry loop is used to wait for writes to become visible
const MAX_QUERY_RETRY_TIME_SEC: u64 = 10;

pub async fn query_until_results(
    sql: impl Into<String>,
    namespace: impl Into<String>,
    connection: Connection,
) -> Vec<RecordBatch> {
    let namespace = namespace.into();
    let sql = sql.into();

    let mut client = influxdb_iox_client::flight::Client::new(connection);

    // This does nothing except test the client handshake implementation.
    client.handshake().await.unwrap();

    let retry_duration = Duration::from_secs(MAX_QUERY_RETRY_TIME_SEC);
    tokio::time::timeout(retry_duration, async move {
        let mut interval = tokio::time::interval(Duration::from_millis(100));
        loop {
            let mut response = match client.perform_query(&namespace, &sql).await {
                Ok(res) => res,
                Err(e) => {
                    println!("Retrying; Got error performing query: {}", e);
                    interval.tick().await;
                    continue;
                }
            };

            let batches = match response.collect().await {
                Ok(batches) => batches,
                Err(e) => {
                    println!("Retrying; Got error running query: {}", e);
                    interval.tick().await;
                    continue;
                }
            };

            // wait for some data to actually arrive
            if batches.is_empty() {
                println!("Retrying: No record results yet");
                interval.tick().await;
                continue;
            }

            return batches;
        }
    })
    .await
    .expect("successfully ran the query in the allotted time")
}

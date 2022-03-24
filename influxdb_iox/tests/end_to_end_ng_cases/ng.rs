use assert_cmd::Command;
use http::StatusCode;
use predicates::prelude::*;
use test_helpers_end_to_end_ng::{
    maybe_skip_integration, rand_name, write_to_router, ServerFixture, ServerType, TestConfig,
};

use arrow_util::assert_batches_sorted_eq;
use data_types2::{IngesterQueryRequest, SequencerId};
use tempfile::TempDir;

#[tokio::test]
async fn router2_through_ingester() {
    let database_url = maybe_skip_integration!();

    let write_buffer_dir = TempDir::new().unwrap();
    let write_buffer_string = write_buffer_dir.path().display().to_string();
    let n_sequencers = 1;
    let sequencer_id = SequencerId::new(1);
    let org = rand_name();
    let bucket = rand_name();
    let namespace = format!("{}_{}", org, bucket);
    let table_name = "mytable";

    // Set up router2 ====================================

    let test_config = TestConfig::new(ServerType::Router2)
        .with_postgres_catalog(&database_url)
        .with_env("INFLUXDB_IOX_WRITE_BUFFER_TYPE", "file")
        .with_env(
            "INFLUXDB_IOX_WRITE_BUFFER_AUTO_CREATE_TOPICS",
            n_sequencers.to_string(),
        )
        .with_env("INFLUXDB_IOX_WRITE_BUFFER_ADDR", &write_buffer_string);
    let router2 = ServerFixture::create_single_use_with_config(test_config).await;

    // Write some data into the v2 HTTP API ==============
    let lp = format!("{},tag1=A,tag2=B val=42i 123456", table_name);

    let response = write_to_router(lp, org, bucket, router2.server.router_http_base()).await;

    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    // Set up ingester ===================================

    let test_config = TestConfig::new(ServerType::Ingester)
        .with_postgres_catalog(&database_url)
        .with_env("INFLUXDB_IOX_WRITE_BUFFER_TYPE", "file")
        .with_env("INFLUXDB_IOX_PAUSE_INGEST_SIZE_BYTES", "20")
        .with_env("INFLUXDB_IOX_PERSIST_MEMORY_THRESHOLD_BYTES", "10")
        .with_env("INFLUXDB_IOX_WRITE_BUFFER_ADDR", &write_buffer_string)
        .with_env("INFLUXDB_IOX_WRITE_BUFFER_PARTITION_RANGE_START", "0")
        .with_env("INFLUXDB_IOX_WRITE_BUFFER_PARTITION_RANGE_END", "0")
        .with_env(
            "INFLUXDB_IOX_WRITE_BUFFER_AUTO_CREATE_TOPICS",
            n_sequencers.to_string(),
        );
    let ingester = ServerFixture::create_single_use_with_config(test_config).await;

    let mut querier_flight =
        querier::flight::Client::new(ingester.server.ingester_grpc_connection());

    let query = IngesterQueryRequest::new(
        namespace.clone(),
        sequencer_id,
        table_name.into(),
        vec![],
        Some(::predicate::EMPTY_PREDICATE),
    );

    let mut performed_query = querier_flight.perform_query(query).await.unwrap();

    assert!(performed_query.parquet_max_sequence_number.is_none());

    let query_results = performed_query.collect().await.unwrap();

    let expected = [
        "+------+------+--------------------------------+-----+",
        "| tag1 | tag2 | time                           | val |",
        "+------+------+--------------------------------+-----+",
        "| A    | B    | 1970-01-01T00:00:00.000123456Z | 42  |",
        "+------+------+--------------------------------+-----+",
    ];
    assert_batches_sorted_eq!(&expected, &query_results);

    // Validate the output of the schema CLI command
    Command::cargo_bin("influxdb_iox")
        .unwrap()
        .arg("-h")
        .arg(router2.server.router_grpc_base().as_ref())
        .arg("schema")
        .arg("get")
        .arg(namespace)
        .assert()
        .success()
        .stdout(
            predicate::str::contains("mytable")
                .and(predicate::str::contains("tag1"))
                .and(predicate::str::contains("val")),
        );
}

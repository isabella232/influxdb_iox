use crate::common::server_fixture::{ServerFixture, ServerType, TestConfig, DEFAULT_SERVER_ID};
use arrow::{
    array::{ArrayRef, Float64Array, StringArray, TimestampNanosecondArray},
    record_batch::RecordBatch,
};
use data_types::{
    chunk_metadata::{ChunkStorage, ChunkSummary},
    names::org_and_bucket_to_database,
    DatabaseName,
};
use generated_types::{
    google::protobuf::Empty,
    influxdata::iox::{management::v1::*, write_buffer::v1::WriteBufferCreationConfig},
    ReadSource, TimestampRange,
};
use influxdb_iox_client::{
    connection::Connection,
    management::{
        self,
        generated_types::{partition_template, WriteBufferConnection},
    },
};
use prost::Message;
use rand::{
    distributions::{Alphanumeric, Standard},
    thread_rng, Rng,
};
use std::{
    collections::HashMap,
    convert::TryInto,
    num::NonZeroU32,
    path::{Path, PathBuf},
    str,
    sync::Arc,
    time::Duration,
    time::SystemTime,
    u32,
};
use tempfile::TempDir;
use test_helpers::assert_contains;
use time::SystemProvider;
use uuid::Uuid;
use write_buffer::{
    core::{WriteBufferReading, WriteBufferWriting},
    file::{FileBufferConsumer, FileBufferProducer},
};

type Error = Box<dyn std::error::Error + Send + Sync + 'static>;
type Result<T, E = Error> = std::result::Result<T, E>;

/// A test fixture used for working with the influxdb v2 data model
/// (storage gRPC api and v2 write api).
///
/// Each scenario is assigned a a random org and bucket id to ensure
/// tests do not interfere with one another
#[derive(Debug)]
pub struct Scenario {
    org_id: String,
    bucket_id: String,
    ns_since_epoch: i64,
}

impl Scenario {
    /// Create a new `Scenario` with a random org_id and bucket_id
    pub fn new() -> Self {
        let ns_since_epoch = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("System time should have been after the epoch")
            .as_nanos()
            .try_into()
            .expect("Unable to represent system time");

        Self {
            ns_since_epoch,
            org_id: rand_id(),
            bucket_id: rand_id(),
        }
    }

    pub fn org_id_str(&self) -> &str {
        &self.org_id
    }

    pub fn bucket_id_str(&self) -> &str {
        &self.bucket_id
    }

    pub fn org_id(&self) -> u64 {
        u64::from_str_radix(&self.org_id, 16).unwrap()
    }

    pub fn bucket_id(&self) -> u64 {
        u64::from_str_radix(&self.bucket_id, 16).unwrap()
    }

    pub fn database_name(&self) -> DatabaseName<'_> {
        org_and_bucket_to_database(&self.org_id, &self.bucket_id).unwrap()
    }

    pub fn ns_since_epoch(&self) -> i64 {
        self.ns_since_epoch
    }

    pub fn read_source(&self) -> Option<generated_types::google::protobuf::Any> {
        let partition_id = u64::from(u32::MAX);
        let read_source = ReadSource {
            org_id: self.org_id(),
            bucket_id: self.bucket_id(),
            partition_id,
        };

        let mut d = bytes::BytesMut::new();
        read_source.encode(&mut d).unwrap();
        let read_source = generated_types::google::protobuf::Any {
            type_url: "/TODO".to_string(),
            value: d.freeze(),
        };

        Some(read_source)
    }

    pub fn timestamp_range(&self) -> Option<TimestampRange> {
        Some(TimestampRange {
            start: self.ns_since_epoch(),
            end: self.ns_since_epoch() + 10,
        })
    }

    /// returns a function suitable for normalizing output that
    /// contains org and bucket ids.
    ///
    /// Specifically, the function will replace all instances of
    /// `org_id` with `XXXXXXXXXXXXXXXX` and the `bucket_id` with a
    /// `YYYYYYYYYYYYYY`, and the read source with `ZZZZZZZZZZZZZZZZ`
    pub fn normalizer(&self) -> impl Fn(&str) -> String {
        let org_id = self.org_id.clone();
        let bucket_id = self.bucket_id.clone();

        // also, the actual gRPC request has the org id encoded in the ReadSource,
        // \"value\": \"CLmSwbj3opLLdRCWrJ2bgoeRw5kBGP////8P\" |",
        let read_source_value = self.read_source().unwrap().value;
        let read_source_value = base64::encode(&read_source_value);

        move |s: &str| {
            s.replace(&org_id, "XXXXXXXXXXXXXXXX")
                .replace(&bucket_id, "YYYYYYYYYYYYYY")
                .replace(&read_source_value, "ZZZZZZZZZZZZZZZZ")
        }
    }

    /// Creates the database on the server for this scenario,
    /// returning (name, uuid)
    pub async fn create_database(
        &self,
        client: &mut management::Client,
    ) -> (DatabaseName<'_>, Uuid) {
        let db_name = self.database_name();

        let db_uuid = client
            .create_database(DatabaseRules {
                name: db_name.to_string(),
                lifecycle_rules: Some(Default::default()),
                ..Default::default()
            })
            .await
            .unwrap();

        (db_name, db_uuid)
    }

    pub async fn load_data(&self, client: &mut influxdb_iox_client::write::Client) -> Vec<String> {
        // TODO: make a more extensible way to manage data for tests, such as in
        // external fixture files or with factories.
        let points = vec![
            format!(
                "cpu_load_short,host=server01,region=us-west value=0.64 {}",
                self.ns_since_epoch()
            ),
            format!(
                "cpu_load_short,host=server01 value=27.99 {}",
                self.ns_since_epoch() + 1
            ),
            format!(
                "cpu_load_short,host=server02,region=us-west value=3.89 {}",
                self.ns_since_epoch() + 2
            ),
            format!(
                "cpu_load_short,host=server01,region=us-east value=1234567.891011 {}",
                self.ns_since_epoch() + 3
            ),
            format!(
                "cpu_load_short,host=server01,region=us-west value=0.000003 {}",
                self.ns_since_epoch() + 4
            ),
            format!(
                "system,host=server03 uptime=1303385i {}",
                self.ns_since_epoch() + 5
            ),
            format!(
                "swap,host=server01,name=disk0 in=3i,out=4i {}",
                self.ns_since_epoch() + 6
            ),
            format!("status active=true {}", self.ns_since_epoch() + 7),
            format!("attributes color=\"blue\" {}", self.ns_since_epoch() + 8),
        ];
        self.write_data(client, points.join("\n")).await.unwrap();

        let host_array = StringArray::from(vec![
            Some("server01"),
            Some("server01"),
            Some("server02"),
            Some("server01"),
            Some("server01"),
        ]);
        let region_array = StringArray::from(vec![
            Some("us-west"),
            None,
            Some("us-west"),
            Some("us-east"),
            Some("us-west"),
        ]);
        let time_array = TimestampNanosecondArray::from_vec(
            vec![
                self.ns_since_epoch,
                self.ns_since_epoch + 1,
                self.ns_since_epoch + 2,
                self.ns_since_epoch + 3,
                self.ns_since_epoch + 4,
            ],
            None,
        );
        let value_array = Float64Array::from(vec![0.64, 27.99, 3.89, 1234567.891011, 0.000003]);

        let batch = RecordBatch::try_from_iter_with_nullable(vec![
            ("host", Arc::new(host_array) as ArrayRef, true),
            ("region", Arc::new(region_array), true),
            ("time", Arc::new(time_array), true),
            ("value", Arc::new(value_array), true),
        ])
        .unwrap();

        arrow_util::display::pretty_format_batches(&[batch])
            .unwrap()
            .trim()
            .split('\n')
            .map(|s| s.to_string())
            .collect()
    }

    pub async fn write_data(
        &self,
        client: &mut influxdb_iox_client::write::Client,
        lp_data: impl AsRef<str> + Send,
    ) -> Result<()> {
        client
            .write_lp(&*self.database_name(), lp_data, self.ns_since_epoch())
            .await?;
        Ok(())
    }
}

/// substitutes "ns" --> ns_since_epoch, ns1-->ns_since_epoch+1, etc
pub fn substitute_nanos(ns_since_epoch: i64, lines: &[&str]) -> Vec<String> {
    let substitutions = vec![
        ("ns0", format!("{}", ns_since_epoch)),
        ("ns1", format!("{}", ns_since_epoch + 1)),
        ("ns2", format!("{}", ns_since_epoch + 2)),
        ("ns3", format!("{}", ns_since_epoch + 3)),
        ("ns4", format!("{}", ns_since_epoch + 4)),
        ("ns5", format!("{}", ns_since_epoch + 5)),
        ("ns6", format!("{}", ns_since_epoch + 6)),
    ];

    lines
        .iter()
        .map(|line| {
            let mut line = line.to_string();
            for (from, to) in &substitutions {
                line = line.replace(from, to);
            }
            line
        })
        .collect()
}

/// Return a random string suitable for use as a database name
pub fn rand_name() -> String {
    thread_rng()
        .sample_iter(&Alphanumeric)
        .take(10)
        .map(char::from)
        .collect()
}

// return a random 16 digit string comprised of numbers suitable for
// use as a influxdb2 org_id or bucket_id
pub fn rand_id() -> String {
    thread_rng()
        .sample_iter(&Standard)
        .filter_map(|c: u8| {
            if c.is_ascii_digit() {
                Some(char::from(c))
            } else {
                // discard if out of range
                None
            }
        })
        .take(16)
        .collect()
}

/// Return the path that the database stores data for all databases:
/// `<server_path>/dbs`
pub fn data_dir(server_path: impl AsRef<Path>) -> PathBuf {
    // Assume data layout is <dir>/dbs/<uuid>
    let mut data_dir: PathBuf = server_path.as_ref().into();
    data_dir.push("dbs");
    data_dir
}

/// Return the path that the database with <uuid> stores its data:
/// `<server_path>/dbs/<uuid>`
pub fn db_data_dir(server_path: impl AsRef<Path>, db_uuid: Uuid) -> PathBuf {
    // Assume data layout is <dir>/dbs/<uuid>
    let mut data_dir = data_dir(server_path);
    data_dir.push(db_uuid.to_string());
    data_dir
}

pub struct DatabaseBuilder {
    name: String,
    partition_template: PartitionTemplate,
    lifecycle_rules: LifecycleRules,
    write_buffer: Option<WriteBufferConnection>,
}

impl DatabaseBuilder {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            partition_template: PartitionTemplate {
                parts: vec![partition_template::Part {
                    part: Some(partition_template::part::Part::Table(Empty {})),
                }],
            },
            lifecycle_rules: LifecycleRules {
                buffer_size_soft: 512 * 1024,       // 512K
                buffer_size_hard: 10 * 1024 * 1024, // 10MB
                worker_backoff_millis: 100,
                ..Default::default()
            },
            write_buffer: None,
        }
    }

    pub fn partition_template(mut self, partition_template: PartitionTemplate) -> Self {
        self.partition_template = partition_template;
        self
    }

    pub fn buffer_size_hard(mut self, buffer_size_hard: u64) -> Self {
        self.lifecycle_rules.buffer_size_hard = buffer_size_hard;
        self
    }

    pub fn buffer_size_soft(mut self, buffer_size_soft: u64) -> Self {
        self.lifecycle_rules.buffer_size_soft = buffer_size_soft;
        self
    }

    pub fn persist(mut self, persist: bool) -> Self {
        self.lifecycle_rules.persist = persist;
        self
    }

    pub fn mub_row_threshold(mut self, threshold: u64) -> Self {
        self.lifecycle_rules.mub_row_threshold = threshold;
        self
    }

    pub fn persist_age_threshold_seconds(mut self, threshold: u32) -> Self {
        self.lifecycle_rules.persist_age_threshold_seconds = threshold;
        self
    }

    pub fn persist_row_threshold(mut self, threshold: u64) -> Self {
        self.lifecycle_rules.persist_row_threshold = threshold;
        self
    }

    pub fn late_arrive_window_seconds(mut self, late_arrive_window_seconds: u32) -> Self {
        self.lifecycle_rules.late_arrive_window_seconds = late_arrive_window_seconds;
        self
    }

    pub fn write_buffer(mut self, write_buffer: WriteBufferConnection) -> Self {
        self.write_buffer = Some(write_buffer);
        self
    }

    pub fn worker_backoff_millis(mut self, millis: u64) -> Self {
        self.lifecycle_rules.worker_backoff_millis = millis;
        self
    }

    // Build a database, returning the UUID of the created database
    pub async fn try_build(
        self,
        channel: Connection,
    ) -> Result<Uuid, influxdb_iox_client::error::Error> {
        let mut management_client = management::Client::new(channel);

        management_client
            .create_database(DatabaseRules {
                name: self.name,
                partition_template: Some(self.partition_template),
                lifecycle_rules: Some(self.lifecycle_rules),
                worker_cleanup_avg_sleep: None,
                write_buffer_connection: self.write_buffer,
            })
            .await
    }

    // Build a database
    pub async fn build(self, channel: Connection) -> Uuid {
        self.try_build(channel)
            .await
            .expect("create database failed")
    }
}

/// given a channel to talk with the management api, create a new
/// database with the specified name configured with a 10MB mutable
/// buffer, partitioned on table, returning the UUID of the created database
pub async fn create_readable_database(db_name: impl Into<String>, channel: Connection) -> Uuid {
    DatabaseBuilder::new(db_name.into()).build(channel).await
}

/// given a channel to talk with the management api, create a new
/// database with no mutable buffer configured, no partitioning rules
pub async fn create_unreadable_database(db_name: impl Into<String>, channel: Connection) {
    let mut management_client = management::Client::new(channel);

    let rules = DatabaseRules {
        name: db_name.into(),
        ..Default::default()
    };

    management_client
        .create_database(rules.clone())
        .await
        .expect("create database failed");
}

/// given a channel to talk with the management api, create a new
/// database with the specified name configured with a 10MB mutable
/// buffer, partitioned on table, with some data written into two partitions
pub async fn create_two_partition_database(db_name: impl Into<String>, channel: Connection) {
    let mut write_client = influxdb_iox_client::write::Client::new(channel.clone());

    let db_name = db_name.into();
    create_readable_database(&db_name, channel).await;

    let lp_lines = vec![
        "mem,host=foo free=27875999744i,cached=0i,available_percent=62.2 1591894320000000000",
        "cpu,host=foo running=4i,sleeping=514i,total=519i 1592894310000000000",
    ];

    write_client
        .write_lp(&db_name, lp_lines.join("\n"), 0)
        .await
        .expect("write succeded");
}

/// Wait for the chunks to be in exactly `desired_storages` states
pub async fn wait_for_exact_chunk_states(
    fixture: &ServerFixture,
    db_name: &str,
    mut desired_storages: Vec<ChunkStorage>,
    wait_time: std::time::Duration,
) -> Vec<ChunkSummary> {
    // ensure consistent order
    desired_storages.sort_unstable();

    let fail_message = format!("persisted chunks in exactly {:?}", desired_storages);
    let pred = |chunks: &[ChunkSummary]| {
        let mut actual_storages = chunks.iter().map(|chunk| chunk.storage).collect::<Vec<_>>();
        actual_storages.sort_unstable();

        desired_storages == actual_storages
    };
    wait_for_state(fixture, db_name, pred, fail_message, wait_time).await
}

/// Wait for the predicate to pass
async fn wait_for_state<P>(
    fixture: &ServerFixture,
    db_name: &str,
    mut pred: P,
    fail_message: String,
    wait_time: std::time::Duration,
) -> Vec<ChunkSummary>
where
    P: FnMut(&[ChunkSummary]) -> bool,
{
    let t_start = std::time::Instant::now();

    loop {
        let chunks = list_chunks(fixture, db_name).await;

        if pred(&chunks) {
            return chunks;
        }

        // Log the current status of the chunks
        for chunk in &chunks {
            println!(
                "{:?}: chunk {} partition {} storage: {:?} row_count: {} time_of_last_write: {:?}",
                (t_start.elapsed()),
                chunk.id,
                chunk.partition_key,
                chunk.storage,
                chunk.row_count,
                chunk.time_of_last_write
            );
        }

        if t_start.elapsed() >= wait_time {
            let mut operations = fixture.operations_client().list_operations().await.unwrap();
            operations.sort_by(|a, b| a.operation.name.cmp(&b.operation.name));

            panic!(
                "Could not find {} within {:?}.\nChunks were: {:#?}\nOperations were: {:#?}",
                fail_message, wait_time, chunks, operations
            )
        }

        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
}

// Wait for up to `wait_time` all operations to be complete
pub async fn wait_for_operations_to_complete(
    fixture: &ServerFixture,
    db_name: &str,
    wait_time: std::time::Duration,
) {
    let t_start = std::time::Instant::now();
    let mut operations_client = fixture.operations_client();

    loop {
        let mut operations = operations_client.list_operations().await.unwrap();
        operations.sort_by(|a, b| a.operation.name.cmp(&b.operation.name));

        // if all operations are complete, great!
        let all_ops_done = operations
            .iter()
            .filter(|op| {
                // job name matches
                op.metadata
                    .job
                    .as_ref()
                    .map(|job| job.db_name() == db_name)
                    .unwrap_or(false)
            })
            .all(|op| op.operation.done);

        if all_ops_done {
            println!(
                "All operations for {} complete after {:?}:\n\n{:#?}",
                db_name,
                t_start.elapsed(),
                operations
            );
            return;
        }

        if t_start.elapsed() >= wait_time {
            panic!(
                "Operations for {} did not complete in {:?}:\n\n{:#?}",
                db_name, wait_time, operations
            );
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
}

pub async fn wait_for_database_initialized(
    fixture: &ServerFixture,
    db_name: &str,
    wait_time: std::time::Duration,
) {
    use generated_types::influxdata::iox::management::v1::database_status::DatabaseState;

    let t_start = std::time::Instant::now();
    let mut management_client = fixture.management_client();

    loop {
        let status = management_client.get_server_status().await.unwrap();
        if status
            .database_statuses
            .iter()
            .filter(|status| status.db_name == db_name)
            .all(|status| {
                DatabaseState::from_i32(status.state).unwrap() == DatabaseState::Initialized
            })
        {
            println!("Database {} is initialized", db_name);
            return;
        }

        if t_start.elapsed() >= wait_time {
            panic!(
                "Database {} was not initialized in {:?}:\n\n{:#?}",
                db_name, wait_time, status
            );
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
}

/// Gets the list of ChunkSummaries from the server
pub async fn list_chunks(fixture: &ServerFixture, db_name: &str) -> Vec<ChunkSummary> {
    let mut management_client = fixture.management_client();
    let chunks = management_client.list_chunks(db_name).await.unwrap();
    let mut chunks: Vec<ChunkSummary> = chunks.into_iter().map(|c| c.try_into().unwrap()).collect();
    chunks.sort_by_key(|summary| {
        (
            Arc::clone(&summary.table_name),
            Arc::clone(&summary.partition_key),
            summary.id,
        )
    });
    chunks
}

/// Creates a database with a broken catalog
pub async fn fixture_broken_catalog(db_name: &str) -> ServerFixture {
    let test_config =
        TestConfig::new(ServerType::Database).with_env("INFLUXDB_IOX_WIPE_CATALOG_ON_ERROR", "no");

    let fixture = ServerFixture::create_single_use_with_config(test_config).await;
    fixture
        .deployment_client()
        .update_server_id(NonZeroU32::new(DEFAULT_SERVER_ID).unwrap())
        .await
        .unwrap();
    fixture.wait_server_initialized().await;

    //
    // Create database with corrupted catalog
    //

    let uuid = fixture
        .management_client()
        .create_database(DatabaseRules {
            name: db_name.to_string(),
            ..Default::default()
        })
        .await
        .unwrap();

    //
    // Try to load broken catalog and error
    //
    fixture.poison_catalog(uuid);
    let fixture = fixture.restart_server().await;

    let status = fixture.wait_server_initialized().await;
    assert_eq!(status.database_statuses.len(), 1);

    let load_error = &status.database_statuses[0].error.as_ref().unwrap().message;
    assert_contains!(
        load_error,
        "error loading catalog: Cannot load preserved catalog"
    );

    fixture
}

/// Creates a database that cannot be replayed
pub async fn fixture_replay_broken(db_name: &str, write_buffer_path: &Path) -> ServerFixture {
    let test_config =
        TestConfig::new(ServerType::Database).with_env("INFLUXDB_IOX_SKIP_REPLAY", "no");

    let fixture = ServerFixture::create_single_use_with_config(test_config).await;
    fixture
        .deployment_client()
        .update_server_id(NonZeroU32::new(DEFAULT_SERVER_ID).unwrap())
        .await
        .unwrap();
    fixture.wait_server_initialized().await;

    // Create database
    fixture
        .management_client()
        .create_database(DatabaseRules {
            name: db_name.to_string(),
            write_buffer_connection: Some(WriteBufferConnection {
                r#type: "file".to_string(),
                connection: write_buffer_path.display().to_string(),
                creation_config: Some(WriteBufferCreationConfig {
                    n_sequencers: 1,
                    ..Default::default()
                }),
                ..Default::default()
            }),
            partition_template: Some(PartitionTemplate {
                parts: vec![partition_template::Part {
                    part: Some(partition_template::part::Part::Column(
                        "partition_by".to_string(),
                    )),
                }],
            }),
            lifecycle_rules: Some(LifecycleRules {
                persist: true,
                late_arrive_window_seconds: 1,
                persist_age_threshold_seconds: 3600,
                persist_row_threshold: 2,
                ..Default::default()
            }),
            ..Default::default()
        })
        .await
        .unwrap();

    // ingest data as mixed throughput
    let time_provider = Arc::new(SystemProvider::new());
    let producer = FileBufferProducer::new(
        write_buffer_path,
        db_name,
        Default::default(),
        time_provider,
    )
    .await
    .unwrap();
    let sequencer_id = producer.sequencer_ids().into_iter().next().unwrap();
    let meta1 = producer
        .store_lp(sequencer_id, "table_1,partition_by=a foo=1 10", 0)
        .await
        .unwrap();
    let meta2 = producer
        .store_lp(sequencer_id, "table_1,partition_by=b foo=2 20", 0)
        .await
        .unwrap();
    let meta3 = producer
        .store_lp(sequencer_id, "table_1,partition_by=b foo=3 30", 0)
        .await
        .unwrap();

    // wait for ingest, compaction and persistence
    wait_for_exact_chunk_states(
        &fixture,
        db_name,
        vec![
            // that's the single entry from partition a
            ChunkStorage::ReadBuffer,
            // these are the two entries from partition b that got persisted due to the row limit
            ChunkStorage::ReadBufferAndObjectStore,
        ],
        Duration::from_secs(10),
    )
    .await;

    // add new entry to the end
    producer
        .store_lp(sequencer_id, "table_1,partition_by=c foo=4 40", 0)
        .await
        .unwrap();

    // purge data from write buffer
    write_buffer::file::test_utils::remove_entry(
        write_buffer_path,
        db_name,
        sequencer_id,
        meta1.sequence().unwrap().number,
    )
    .await;
    write_buffer::file::test_utils::remove_entry(
        write_buffer_path,
        db_name,
        sequencer_id,
        meta2.sequence().unwrap().number,
    )
    .await;
    write_buffer::file::test_utils::remove_entry(
        write_buffer_path,
        db_name,
        sequencer_id,
        meta3.sequence().unwrap().number,
    )
    .await;

    // Try to replay and error
    let fixture = fixture.restart_server().await;

    let status = fixture.wait_server_initialized().await;
    assert_eq!(status.database_statuses.len(), 1);

    let load_error = &status.database_statuses[0].error.as_ref().unwrap().message;
    assert_contains!(load_error, "error during replay: Cannot replay");

    fixture
}

pub fn wildcard_router_config(
    db_name: &str,
    write_buffer_path: &Path,
) -> influxdb_iox_client::router::generated_types::Router {
    use influxdb_iox_client::router::generated_types::{
        write_sink::Sink, Matcher, MatcherToShard, Router, ShardConfig, WriteSink, WriteSinkSet,
    };

    let write_buffer_connection = WriteBufferConnection {
        r#type: "file".to_string(),
        connection: write_buffer_path.display().to_string(),
        creation_config: Some(WriteBufferCreationConfig {
            n_sequencers: 1,
            ..Default::default()
        }),
        ..Default::default()
    };
    Router {
        name: db_name.to_string(),
        write_sharder: Some(ShardConfig {
            specific_targets: vec![MatcherToShard {
                matcher: Some(Matcher {
                    table_name_regex: String::from(".*"),
                }),
                shard: 1,
            }],
            hash_ring: None,
        }),
        write_sinks: HashMap::from([(
            1,
            WriteSinkSet {
                sinks: vec![WriteSink {
                    ignore_errors: false,
                    sink: Some(Sink::WriteBuffer(write_buffer_connection)),
                }],
            },
        )]),
        query_sinks: Default::default(),
    }
}

pub async fn create_router_to_write_buffer(
    fixture: &ServerFixture,
    db_name: &str,
) -> (TempDir, Box<dyn WriteBufferReading>) {
    let write_buffer_dir = TempDir::new().unwrap();

    let router_cfg = wildcard_router_config(db_name, write_buffer_dir.path());
    fixture
        .router_client()
        .update_router(router_cfg)
        .await
        .unwrap();

    let write_buffer: Box<dyn WriteBufferReading> = Box::new(
        FileBufferConsumer::new(
            write_buffer_dir.path(),
            db_name,
            Some(&data_types::write_buffer::WriteBufferCreationConfig {
                n_sequencers: NonZeroU32::new(1).unwrap(),
                ..Default::default()
            }),
            None,
        )
        .await
        .unwrap(),
    );

    (write_buffer_dir, write_buffer)
}

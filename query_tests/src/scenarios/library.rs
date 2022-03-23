//! Library of test scenarios that can be used in query_tests
use async_trait::async_trait;
use data_types::{
    delete_predicate::{DeleteExpr, DeletePredicate},
    timestamp::TimestampRange,
};
use db::{
    test_helpers::write_lp,
    utils::{
        count_mutable_buffer_chunks, count_object_store_chunks, count_read_buffer_chunks, make_db,
    },
    LockableChunk, LockablePartition,
};
use query::QueryChunk;

use super::{
    util::{
        all_scenarios_for_one_chunk, make_one_chunk_mub_scenario, make_one_chunk_rub_scenario,
        make_one_rub_or_parquet_chunk_scenario, make_two_chunk_scenarios, rollover_and_load,
    },
    DbScenario, DbSetup,
};

/// No data
#[derive(Debug)]
pub struct ChunkOrder {}
#[async_trait]
impl DbSetup for ChunkOrder {
    async fn make(&self) -> Vec<DbScenario> {
        let partition_key = "1970-01-01T00";
        let table_name = "cpu";

        let db = make_db().await.db;

        // create first chunk: data->MUB->RUB
        write_lp(&db, "cpu,region=west user=1 100");
        assert_eq!(count_mutable_buffer_chunks(&db), 1);
        assert_eq!(count_read_buffer_chunks(&db), 0);
        assert_eq!(count_object_store_chunks(&db), 0);
        db.compact_partition(table_name, partition_key)
            .await
            .unwrap();
        assert_eq!(count_mutable_buffer_chunks(&db), 0);
        assert_eq!(count_read_buffer_chunks(&db), 1);
        assert_eq!(count_object_store_chunks(&db), 0);

        // We prepare a persist, then drop the locks, perform another write, re-acquire locks
        // and start a persist operation. In practice the lifecycle doesn't drop the locks
        // before starting the persist operation, but this allows us to deterministically
        // interleave a persist with a write
        let partition = db.lockable_partition(table_name, partition_key).unwrap();
        let (chunks, flush_handle) = {
            let partition = partition.read();
            let chunks = LockablePartition::chunks(&partition);
            let mut partition = partition.upgrade();
            let flush_handle = LockablePartition::prepare_persist(&mut partition, true).unwrap();

            (chunks, flush_handle)
        };

        // create second chunk: data->MUB
        write_lp(&db, "cpu,region=west user=2 100");
        assert_eq!(count_mutable_buffer_chunks(&db), 1);
        assert_eq!(count_read_buffer_chunks(&db), 1);
        assert_eq!(count_object_store_chunks(&db), 0);

        let tracker = {
            let partition = partition.write();
            let chunks = chunks.iter().map(|chunk| chunk.write()).collect();
            LockablePartition::persist_chunks(partition, chunks, flush_handle).unwrap()
        };

        tracker.join().await;
        assert!(tracker.get_status().result().unwrap().success());

        assert_eq!(count_mutable_buffer_chunks(&db), 1);
        assert_eq!(count_read_buffer_chunks(&db), 1);
        assert_eq!(count_object_store_chunks(&db), 1);

        // Now we have the the following chunks (same partition and table):
        //
        // | ID | order | tag: region | field: user | time |
        // | -- | ----- | ----------- | ----------- | ---- |
        // |  1 |     1 | "west"      |           2 | 100  |
        // |  2 |     0 | "west"      |           1 | 100  |
        //
        // The result after deduplication should be:
        //
        // | tag: region | field: user | time |
        // | ----------- | ----------- | ---- |
        // | "west"      |           2 | 100  |
        //
        // So the query engine must use `order` as a primary key to sort chunks, NOT `id`.

        let scenario = DbScenario {
            scenario_name: "chunks where chunk ID alone cannot be used for ordering".into(),
            db,
        };

        vec![scenario]
    }
}

#[derive(Debug)]
pub struct MeasurementWithMaxTime {}
#[async_trait]
impl DbSetup for MeasurementWithMaxTime {
    async fn make(&self) -> Vec<DbScenario> {
        let partition_key = "2262-04-11T23";

        // This is the maximum timestamp that can be represented in the InfluxDB data model:
        // https://github.com/influxdata/influxdb/blob/540bb66e1381a48a6d1ede4fc3e49c75a7d9f4af/models/time.go#L12-L34
        let max_nano_time = i64::MAX - 1; // 9223372036854775806

        let lp_lines = vec![format!("cpu,host=server01 value=100 {}", max_nano_time)];
        let lp_lines = lp_lines.iter().map(|s| s.as_str()).collect();

        all_scenarios_for_one_chunk(vec![], vec![], lp_lines, "cpu", partition_key).await
    }
}

/// No data
#[derive(Debug)]
pub struct NoData {}
#[async_trait]
impl DbSetup for NoData {
    async fn make(&self) -> Vec<DbScenario> {
        let partition_key = "1970-01-01T00";
        let table_name = "cpu";

        // Scenario 1: No data in the DB yet
        //
        let db = make_db().await.db;
        let scenario1 = DbScenario {
            scenario_name: "New, Empty Database".into(),
            db,
        };

        // Scenario 2: listing partitions (which may create an entry in a map)
        // in an empty database
        //
        let db = make_db().await.db;
        assert_eq!(count_mutable_buffer_chunks(&db), 0);
        assert_eq!(count_read_buffer_chunks(&db), 0);
        assert_eq!(count_object_store_chunks(&db), 0);
        let scenario2 = DbScenario {
            scenario_name: "New, Empty Database after partitions are listed".into(),
            db,
        };

        // Scenario 3: the database has had data loaded into RB and then deleted
        //
        let db = make_db().await.db;
        let data = "cpu,region=west user=23.2 100";
        write_lp(&db, data);
        // move data out of open chunk
        db.rollover_partition(table_name, partition_key)
            .await
            .unwrap();
        assert_eq!(count_mutable_buffer_chunks(&db), 1); //
        assert_eq!(count_read_buffer_chunks(&db), 0); // nothing yet
        assert_eq!(count_object_store_chunks(&db), 0); // nothing yet

        // Now load the closed chunk into the RB
        let chunk_id = db
            .compact_partition(table_name, partition_key)
            .await
            .unwrap()
            .unwrap()
            .id();
        assert_eq!(count_mutable_buffer_chunks(&db), 0); // open chunk only
        assert_eq!(count_read_buffer_chunks(&db), 1); // close chunk only
        assert_eq!(count_object_store_chunks(&db), 0); // nothing yet

        // drop chunk
        db.drop_chunk(table_name, partition_key, chunk_id)
            .await
            .unwrap();

        assert_eq!(count_mutable_buffer_chunks(&db), 0); // open chunk only
        assert_eq!(count_read_buffer_chunks(&db), 0); // nothing after dropping chunk 0
        assert_eq!(count_object_store_chunks(&db), 0); // still nothing

        let scenario3 = DbScenario {
            scenario_name: "Empty Database after drop chunk that is in read buffer".into(),
            db,
        };

        // Scenario 4: the database has had data loaded into RB & Object Store and then deleted
        //
        let db = make_db().await.db;
        let data = "cpu,region=west user=23.2 100";
        write_lp(&db, data);
        // move data out of open chunk
        db.rollover_partition(table_name, partition_key)
            .await
            .unwrap();
        assert_eq!(count_mutable_buffer_chunks(&db), 1); // 1 open chunk
        assert_eq!(count_read_buffer_chunks(&db), 0); // nothing yet
        assert_eq!(count_object_store_chunks(&db), 0); // nothing yet

        // Now load the closed chunk into the RB
        db.compact_partition(table_name, partition_key)
            .await
            .unwrap();
        assert_eq!(count_mutable_buffer_chunks(&db), 0); // open chunk only
        assert_eq!(count_read_buffer_chunks(&db), 1); // close chunk only
        assert_eq!(count_object_store_chunks(&db), 0); // nothing yet

        // Now write the data in RB to object store but keep it in RB
        let chunk_id = db
            .persist_partition("cpu", partition_key, true)
            .await
            .unwrap()
            .unwrap()
            .id();
        assert_eq!(count_mutable_buffer_chunks(&db), 0); // open chunk only
        assert_eq!(count_read_buffer_chunks(&db), 1); // closed chunk only
        assert_eq!(count_object_store_chunks(&db), 1); // close chunk only

        // drop chunk
        db.drop_chunk(table_name, partition_key, chunk_id)
            .await
            .unwrap();

        assert_eq!(count_mutable_buffer_chunks(&db), 0);
        assert_eq!(count_read_buffer_chunks(&db), 0);
        assert_eq!(count_object_store_chunks(&db), 0);

        let scenario4 = DbScenario {
            scenario_name:
                "Empty Database after drop chunk that is in both read buffer and object store"
                    .into(),
            db,
        };

        vec![scenario1, scenario2, scenario3, scenario4]
    }
}

/// a measurement with timestamps in 2021
#[derive(Debug)]
pub struct OneMeasurementRealisticTimes {}
#[async_trait]
impl DbSetup for OneMeasurementRealisticTimes {
    async fn make(&self) -> Vec<DbScenario> {
        let partition_key = "2021-07-20T19";

        let lp_lines = vec![
            "cpu,region=west user=23.2 1626809330000000000",
            "cpu,region=west user=21.0 1626809430000000000",
        ];

        // return all possible scenarios a chunk: MUB open, MUB frozen, RUB, RUB & OS, OS
        all_scenarios_for_one_chunk(vec![], vec![], lp_lines, "cpu", partition_key).await
    }
}

#[derive(Debug)]
pub struct OneMeasurementNoTags {}
#[async_trait]
impl DbSetup for OneMeasurementNoTags {
    async fn make(&self) -> Vec<DbScenario> {
        let partition_key = "1970-01-01T00";

        let lp_lines = vec![
            "h2o temp=70.4 100",
            "h2o temp=72.4 250",
            "h2o temp=50.4 200",
            "h2o level=200.0 300",
        ];

        // return all possible scenarios a chunk: MUB open, MUB frozen, RUB, RUB & OS, OS
        all_scenarios_for_one_chunk(vec![], vec![], lp_lines, "h2o", partition_key).await
    }
}

#[derive(Debug)]
pub struct OneMeasurementManyNullTags {}
#[async_trait]
impl DbSetup for OneMeasurementManyNullTags {
    async fn make(&self) -> Vec<DbScenario> {
        let partition_key = "1970-01-01T00";

        let lp_lines = vec![
            "h2o,state=CA,city=LA,county=LA temp=70.4 100",
            "h2o,state=MA,city=Boston,county=Suffolk temp=72.4 250",
            "h2o,state=MA,city=Boston temp=50.4 200",
            "h2o,state=CA temp=79.0 300",
            "h2o,state=NY temp=60.8 400",
            "h2o,state=NY,city=NYC temp=61.0 500",
            "h2o,state=NY,city=NYC,borough=Brooklyn temp=61.0 600",
        ];

        // return all possible scenarios a chunk: MUB open, MUB frozen, RUB, RUB & OS, OS
        all_scenarios_for_one_chunk(vec![], vec![], lp_lines, "cpu", partition_key).await
    }
}

#[derive(Debug)]
pub struct OneMeasurementManyNullTagsWithDelete {}
#[async_trait]
impl DbSetup for OneMeasurementManyNullTagsWithDelete {
    async fn make(&self) -> Vec<DbScenario> {
        let partition_key = "1970-01-01T00";

        let lp_lines = vec![
            "h2o,state=CA,city=LA,county=LA temp=70.4 100",
            "h2o,state=MA,city=Boston,county=Suffolk temp=72.4 250",
            "h2o,state=MA,city=Boston temp=50.4 200",
            "h2o,state=CA temp=79.0 300",
            "h2o,state=NY temp=60.8 400",
            "h2o,state=NY,city=NYC temp=61.0 500",
            "h2o,state=NY,city=NYC,borough=Brooklyn temp=61.0 600",
        ];

        // pred: delete from h2o where 400 <= time <= 602 and state=NY
        // 3 rows of h2o & NY state will be deleted
        let delete_table_name = "h2o";
        let pred = DeletePredicate {
            range: TimestampRange::new(400, 602),
            exprs: vec![DeleteExpr::new(
                "state".to_string(),
                data_types::delete_predicate::Op::Eq,
                data_types::delete_predicate::Scalar::String(("NY").to_string()),
            )],
        };

        all_scenarios_for_one_chunk(
            vec![&pred],
            vec![],
            lp_lines,
            delete_table_name,
            partition_key,
        )
        .await
    }
}

#[derive(Debug)]
pub struct OneMeasurementManyNullTagsWithDeleteAll {}
#[async_trait]
impl DbSetup for OneMeasurementManyNullTagsWithDeleteAll {
    async fn make(&self) -> Vec<DbScenario> {
        let partition_key = "1970-01-01T00";

        let lp_lines = vec![
            "h2o,state=CA,city=LA,county=LA temp=70.4 100",
            "h2o,state=MA,city=Boston,county=Suffolk temp=72.4 250",
            "h2o,state=MA,city=Boston temp=50.4 200",
            "h2o,state=CA temp=79.0 300",
            "h2o,state=NY temp=60.8 400",
            "h2o,state=NY,city=NYC temp=61.0 500",
            "h2o,state=NY,city=NYC,borough=Brooklyn temp=61.0 600",
        ];

        // pred: delete from h2o where 100 <= time <= 602
        // all rows of h2o  will be deleted
        let delete_table_name = "h2o";
        let pred = DeletePredicate {
            range: TimestampRange::new(100, 602),
            exprs: vec![],
        };

        all_scenarios_for_one_chunk(
            vec![&pred],
            vec![],
            lp_lines,
            delete_table_name,
            partition_key,
        )
        .await
    }
}

/// Two measurements data in a single mutable buffer chunk
#[derive(Debug)]
pub struct TwoMeasurementsMubScenario {}
#[async_trait]
impl DbSetup for TwoMeasurementsMubScenario {
    async fn make(&self) -> Vec<DbScenario> {
        let lp_lines = vec![
            "cpu,region=west user=23.2 100",
            "cpu,region=west user=21.0 150",
            "disk,region=east bytes=99i 200",
        ];

        make_one_chunk_mub_scenario(&lp_lines.join("\n")).await
    }
}

/// Two measurements data in a single read buffer chunk
#[derive(Debug)]
pub struct TwoMeasurementsRubScenario {}
#[async_trait]
impl DbSetup for TwoMeasurementsRubScenario {
    async fn make(&self) -> Vec<DbScenario> {
        let partition_key = "1970-01-01T00";

        let lp_lines = vec![
            "cpu,region=west user=23.2 100",
            "cpu,region=west user=21.0 150",
            "disk,region=east bytes=99i 200",
        ];

        make_one_chunk_rub_scenario(partition_key, &lp_lines.join("\n")).await
    }
}

/// Two measurements data in different chunk scenarios
#[derive(Debug)]
pub struct TwoMeasurements {}
#[async_trait]
impl DbSetup for TwoMeasurements {
    async fn make(&self) -> Vec<DbScenario> {
        let partition_key = "1970-01-01T00";

        let lp_lines = vec![
            "cpu,region=west user=23.2 100",
            "cpu,region=west user=21.0 150",
            "disk,region=east bytes=99i 200",
        ];

        // return all possible scenarios a chunk: MUB open, MUB frozen, RUB, RUB & OS, OS
        all_scenarios_for_one_chunk(vec![], vec![], lp_lines, "cpu", partition_key).await
    }
}

/// Two measurements data in different chunk scenarios
/// with one delete applied at different stages of the chunk
#[derive(Debug)]
pub struct TwoMeasurementsWithDelete {}
#[async_trait]
impl DbSetup for TwoMeasurementsWithDelete {
    async fn make(&self) -> Vec<DbScenario> {
        let partition_key = "1970-01-01T00";

        let lp_lines = vec![
            "cpu,region=west user=23.2 100",
            "cpu,region=west user=21.0 150",
            "disk,region=east bytes=99i 200",
        ];

        // pred: delete from cpu where 120 <= time <= 160 and region="west"
        // delete 1 row from cpu with timestamp 150
        let table_name = "cpu";
        let pred = DeletePredicate {
            range: TimestampRange::new(120, 160),
            exprs: vec![DeleteExpr::new(
                "region".to_string(),
                data_types::delete_predicate::Op::Eq,
                data_types::delete_predicate::Scalar::String("west".to_string()),
            )],
        };

        // return all possible combination scenarios of a chunk stage and when the delete predicates are applied
        all_scenarios_for_one_chunk(vec![&pred], vec![], lp_lines, table_name, partition_key).await
    }
}

/// Two measurements data in different chunk scenarios
/// with 2 deletes that remove all data from one table
#[derive(Debug)]
pub struct TwoMeasurementsWithDeleteAll {}
#[async_trait]
impl DbSetup for TwoMeasurementsWithDeleteAll {
    async fn make(&self) -> Vec<DbScenario> {
        let partition_key = "1970-01-01T00";

        let lp_lines = vec![
            "cpu,region=west user=23.2 100",
            "cpu,region=west user=21.0 150",
            "disk,region=east bytes=99i 200",
        ];

        // pred: delete from cpu where 120 <= time <= 160 and region="west"
        // which will delete second row of the cpu
        let table_name = "cpu";
        let pred1 = DeletePredicate {
            range: TimestampRange::new(120, 160),
            exprs: vec![DeleteExpr::new(
                "region".to_string(),
                data_types::delete_predicate::Op::Eq,
                data_types::delete_predicate::Scalar::String("west".to_string()),
            )],
        };

        // delete the first row of the cpu
        let pred2 = DeletePredicate {
            range: TimestampRange::new(0, 110),
            exprs: vec![],
        };

        // return all possible combination scenarios of a chunk stage and when the delete predicates are applied
        all_scenarios_for_one_chunk(
            vec![&pred1],
            vec![&pred2],
            lp_lines,
            table_name,
            partition_key,
        )
        .await
    }
}

#[derive(Debug)]
pub struct TwoMeasurementsUnsignedTypeMubScenario {}
#[async_trait]
impl DbSetup for TwoMeasurementsUnsignedTypeMubScenario {
    async fn make(&self) -> Vec<DbScenario> {
        let lp_lines = vec![
            "restaurant,town=andover count=40000u 100",
            "restaurant,town=reading count=632u 120",
            "school,town=reading count=17u 150",
            "school,town=andover count=25u 160",
        ];

        make_one_chunk_mub_scenario(&lp_lines.join("\n")).await
    }
}

#[derive(Debug)]
pub struct TwoMeasurementsUnsignedType {}
#[async_trait]
impl DbSetup for TwoMeasurementsUnsignedType {
    async fn make(&self) -> Vec<DbScenario> {
        let partition_key = "1970-01-01T00";

        let lp_lines = vec![
            "restaurant,town=andover count=40000u 100",
            "restaurant,town=reading count=632u 120",
            "school,town=reading count=17u 150",
            "school,town=andover count=25u 160",
        ];

        all_scenarios_for_one_chunk(vec![], vec![], lp_lines, "restaurant", partition_key).await
    }
}

#[derive(Debug)]
pub struct TwoMeasurementsPredicatePushDown {}
#[async_trait]
impl DbSetup for TwoMeasurementsPredicatePushDown {
    async fn make(&self) -> Vec<DbScenario> {
        let partition_key = "1970-01-01T00";

        let lp_lines = vec![
            "restaurant,town=andover count=40000u,system=5.0 100",
            "restaurant,town=reading count=632u,system=5.0 120",
            "restaurant,town=bedford count=189u,system=7.0 110",
            "restaurant,town=tewsbury count=471u,system=6.0 110",
            "restaurant,town=lexington count=372u,system=5.0 100",
            "restaurant,town=lawrence count=872u,system=6.0 110",
            "restaurant,town=reading count=632u,system=6.0 130",
            "school,town=reading count=17u,system=6.0 150",
            "school,town=andover count=25u,system=6.0 160",
        ];

        make_one_rub_or_parquet_chunk_scenario(partition_key, &lp_lines.join("\n")).await
    }
}

/// Single measurement that has several different chunks with
/// different (but compatible) schema
#[derive(Debug)]
pub struct MultiChunkSchemaMerge {}
#[async_trait]
impl DbSetup for MultiChunkSchemaMerge {
    async fn make(&self) -> Vec<DbScenario> {
        let partition_key = "1970-01-01T00";

        let lp_lines1 = vec![
            "cpu,region=west user=23.2,system=5.0 100",
            "cpu,region=west user=21.0,system=6.0 150",
        ];
        let lp_lines2 = [
            "cpu,region=east,host=foo user=23.2 100",
            "cpu,region=west,host=bar user=21.0 250",
        ];

        make_two_chunk_scenarios(partition_key, &lp_lines1.join("\n"), &lp_lines2.join("\n")).await
    }
}

/// Two measurements data with many null values
#[derive(Debug)]
pub struct TwoMeasurementsManyNulls {}
#[async_trait]
impl DbSetup for TwoMeasurementsManyNulls {
    async fn make(&self) -> Vec<DbScenario> {
        let partition_key = "1970-01-01T00";

        let lp_lines1 = vec![
            "h2o,state=CA,city=LA,county=LA temp=70.4 100",
            "h2o,state=MA,city=Boston,county=Suffolk temp=72.4 250",
            "o2,state=MA,city=Boston temp=50.4 200",
            "o2,state=CA temp=79.0 300\n",
        ];
        let lp_lines2 = [
            "o2,state=NY temp=60.8 400",
            "o2,state=NY,city=NYC temp=61.0 500",
            "o2,state=NY,city=NYC,borough=Brooklyn temp=61.0 600",
        ];

        make_two_chunk_scenarios(partition_key, &lp_lines1.join("\n"), &lp_lines2.join("\n")).await
    }
}

#[derive(Debug)]
pub struct TwoMeasurementsManyFields {}
#[async_trait]
impl DbSetup for TwoMeasurementsManyFields {
    async fn make(&self) -> Vec<DbScenario> {
        let partition_key = "1970-01-01T00";

        let lp_lines1 = vec![
            "h2o,state=MA,city=Boston temp=70.4 50",
            "h2o,state=MA,city=Boston other_temp=70.4 250",
            "h2o,state=CA,city=Boston other_temp=72.4 350",
            "o2,state=MA,city=Boston temp=53.4,reading=51 50",
            "o2,state=CA temp=79.0 300",
        ];
        let lp_lines2 = vec!["h2o,state=MA,city=Boston temp=70.4,moisture=43.0 100000"];

        make_two_chunk_scenarios(partition_key, &lp_lines1.join("\n"), &lp_lines2.join("\n")).await
    }
}

#[derive(Debug)]
/// This has a single chunk for queries that check the state of the system
pub struct TwoMeasurementsManyFieldsOneChunk {}
#[async_trait]
impl DbSetup for TwoMeasurementsManyFieldsOneChunk {
    async fn make(&self) -> Vec<DbScenario> {
        let partition_key = "1970-01-01T00";

        let lp_lines = vec![
            "h2o,state=MA,city=Boston temp=70.4 50",
            "h2o,state=MA,city=Boston other_temp=70.4 250",
            "h2o,state=CA,city=Boston other_temp=72.4 350",
            "o2,state=MA,city=Boston temp=53.4,reading=51 50",
            "o2,state=CA temp=79.0 300",
        ];

        all_scenarios_for_one_chunk(vec![], vec![], lp_lines, "h2o", partition_key).await
    }
}

#[derive(Debug)]
/// This has a single chunk for queries that check the state of the system
///
/// This scenario is OG-specific and can be used for `EXPLAIN` plans and system tables.
pub struct OldTwoMeasurementsManyFieldsOneRubChunk {}
#[async_trait]
impl DbSetup for OldTwoMeasurementsManyFieldsOneRubChunk {
    async fn make(&self) -> Vec<DbScenario> {
        let db = make_db().await.db;
        let partition_key = "1970-01-01T00";

        let lp_lines = vec![
            "h2o,state=MA,city=Boston temp=70.4 50",
            "h2o,state=MA,city=Boston other_temp=70.4 250",
            "h2o,state=CA,city=Boston other_temp=72.4 350",
            "o2,state=MA,city=Boston temp=53.4,reading=51 50",
            "o2,state=CA temp=79.0 300",
        ];

        write_lp(&db, &lp_lines.join("\n"));

        // move all data to RUB
        db.compact_open_chunk("h2o", partition_key).await.unwrap();

        vec![DbScenario {
            scenario_name: "Data in single chunk of read buffer".into(),
            db,
        }]
    }
}

#[derive(Debug)]
/// This has two chunks for queries that check the state of the system
pub struct TwoMeasurementsManyFieldsTwoChunks {}
#[async_trait]
impl DbSetup for TwoMeasurementsManyFieldsTwoChunks {
    async fn make(&self) -> Vec<DbScenario> {
        let db = make_db().await.db;

        let partition_key = "1970-01-01T00";

        let lp_lines = vec![
            "h2o,state=MA,city=Boston temp=70.4 50",
            "h2o,state=MA,city=Boston other_temp=70.4 250",
        ];
        write_lp(&db, &lp_lines.join("\n"));
        db.compact_partition("h2o", partition_key).await.unwrap();

        let lp_lines = vec![
            "h2o,state=CA,city=Boston other_temp=72.4 350",
            "o2,state=MA,city=Boston temp=53.4,reading=51 50",
            "o2,state=CA temp=79.0 300",
        ];
        write_lp(&db, &lp_lines.join("\n"));

        assert_eq!(count_mutable_buffer_chunks(&db), 2);
        assert_eq!(count_read_buffer_chunks(&db), 1);
        assert_eq!(count_object_store_chunks(&db), 0);

        vec![DbScenario {
            scenario_name: "Data in two open mutable buffer chunks per table and read buffer"
                .into(),
            db,
        }]
    }
}

#[derive(Debug)]
/// This has two chunks with different tag/key sets for queries whose columns not include keys
pub struct OneMeasurementTwoChunksDifferentTagSet {}
#[async_trait]
impl DbSetup for OneMeasurementTwoChunksDifferentTagSet {
    async fn make(&self) -> Vec<DbScenario> {
        let db = make_db().await.db;

        let partition_key = "1970-01-01T00";

        // tag: state
        let lp_lines = vec![
            "h2o,state=MA temp=70.4 50",
            "h2o,state=MA other_temp=70.4 250",
        ];
        write_lp(&db, &lp_lines.join("\n"));
        db.compact_partition("h2o", partition_key).await.unwrap();

        // tag: city
        let lp_lines = vec![
            "h2o,city=Boston other_temp=72.4 350",
            "h2o,city=Boston temp=53.4,reading=51 50",
        ];
        write_lp(&db, &lp_lines.join("\n"));
        db.compact_open_chunk("h2o", partition_key).await.unwrap();

        assert_eq!(count_mutable_buffer_chunks(&db), 0);
        assert_eq!(count_read_buffer_chunks(&db), 2);
        assert_eq!(count_object_store_chunks(&db), 0);

        vec![DbScenario {
            scenario_name: "2 chunks in read buffer".into(),
            db,
        }]
    }
}

#[derive(Debug)]
/// Setup for four chunks with duplicates for deduplicate tests
pub struct OneMeasurementFourChunksWithDuplicates {}
#[async_trait]
impl DbSetup for OneMeasurementFourChunksWithDuplicates {
    async fn make(&self) -> Vec<DbScenario> {
        let db = make_db().await.db;

        let partition_key = "1970-01-01T00";

        // Chunk 1:
        //  . time range: 50-250
        //  . no duplicates in its own chunk
        let lp_lines = vec![
            "h2o,state=MA,city=Boston min_temp=70.4 50",
            "h2o,state=MA,city=Bedford min_temp=71.59 150",
            "h2o,state=MA,city=Boston max_temp=75.4 250",
            "h2o,state=MA,city=Andover max_temp=69.2, 250",
        ];
        write_lp(&db, &lp_lines.join("\n"));
        db.compact_open_chunk("h2o", partition_key).await.unwrap();

        // Chunk 2: overlaps with chunk 1
        //  . time range: 150 - 300
        //  . no duplicates in its own chunk
        let lp_lines = vec![
            "h2o,state=MA,city=Bedford max_temp=78.75,area=742u 150", // new field (area) and update available NULL (max_temp)
            "h2o,state=MA,city=Boston min_temp=65.4 250",             // update min_temp from NULL
            "h2o,state=MA,city=Reading min_temp=53.4, 250",
            "h2o,state=CA,city=SF min_temp=79.0,max_temp=87.2,area=500u 300",
            "h2o,state=CA,city=SJ min_temp=78.5,max_temp=88.0 300",
            "h2o,state=CA,city=SJ min_temp=75.5,max_temp=84.08 350",
        ];
        write_lp(&db, &lp_lines.join("\n"));
        db.compact_open_chunk("h2o", partition_key).await.unwrap();

        // Chunk 3: no overlap
        //  . time range: 400 - 500
        //  . duplicates in its own chunk
        let lp_lines = vec![
            "h2o,state=MA,city=Bedford max_temp=80.75,area=742u 400",
            "h2o,state=MA,city=Boston min_temp=68.4 400",
            "h2o,state=MA,city=Bedford min_temp=65.22,area=750u 400", // duplicate
            "h2o,state=MA,city=Boston min_temp=65.40,max_temp=82.67 400", // duplicate
            "h2o,state=CA,city=SJ min_temp=77.0,max_temp=90.7 450",
            "h2o,state=CA,city=SJ min_temp=69.5,max_temp=88.2 500",
        ];
        write_lp(&db, &lp_lines.join("\n"));
        db.compact_open_chunk("h2o", partition_key).await.unwrap();

        // Chunk 4: no overlap
        //  . time range: 600 - 700
        //  . no duplicates
        let lp_lines = vec![
            "h2o,state=MA,city=Bedford max_temp=88.75,area=742u 600",
            "h2o,state=MA,city=Boston min_temp=67.4 600",
            "h2o,state=MA,city=Reading min_temp=60.4, 600",
            "h2o,state=CA,city=SF min_temp=68.4,max_temp=85.7,area=500u 650",
            "h2o,state=CA,city=SJ min_temp=69.5,max_temp=89.2 650",
            "h2o,state=CA,city=SJ min_temp=75.5,max_temp=84.08 700",
        ];
        write_lp(&db, &lp_lines.join("\n"));
        db.compact_open_chunk("h2o", partition_key).await.unwrap();

        assert_eq!(count_mutable_buffer_chunks(&db), 0);
        assert_eq!(count_read_buffer_chunks(&db), 4);
        assert_eq!(count_object_store_chunks(&db), 0);

        vec![DbScenario {
            scenario_name: "Data in four chunks with duplicates".into(),
            db,
        }]
    }
}

#[derive(Debug)]
/// This has a single scenario with all the life cycle operations to
/// test queries that depend on that
pub struct TwoMeasurementsManyFieldsLifecycle {}
#[async_trait]
impl DbSetup for TwoMeasurementsManyFieldsLifecycle {
    async fn make(&self) -> Vec<DbScenario> {
        let partition_key = "1970-01-01T00";

        let db = make_db().await.db;

        write_lp(
            &db,
            &vec![
                "h2o,state=MA,city=Boston temp=70.4 50",
                "h2o,state=MA,city=Boston other_temp=70.4 250",
            ]
            .join("\n"),
        );

        db.compact_open_chunk("h2o", partition_key).await.unwrap();

        db.persist_partition("h2o", partition_key, true)
            .await
            .unwrap();

        write_lp(
            &db,
            &vec!["h2o,state=CA,city=Boston other_temp=72.4 350"].join("\n"),
        );

        assert_eq!(count_mutable_buffer_chunks(&db), 1);
        assert_eq!(count_read_buffer_chunks(&db), 1);
        assert_eq!(count_object_store_chunks(&db), 1);

        vec![DbScenario {
            scenario_name: "Data in parquet, RUB, and MUB".into(),
            db,
        }]
    }
}

#[derive(Debug)]
pub struct OneMeasurementManyFields {}
#[async_trait]
impl DbSetup for OneMeasurementManyFields {
    async fn make(&self) -> Vec<DbScenario> {
        let partition_key = "1970-01-01T00";

        // Order this so field3 comes before field2
        // (and thus the columns need to get reordered)
        let lp_lines = vec![
            "h2o,tag1=foo,tag2=bar field1=70.6,field3=2 100",
            "h2o,tag1=foo,tag2=bar field1=70.4,field2=\"ss\" 100",
            "h2o,tag1=foo,tag2=bar field1=70.5,field2=\"ss\" 100",
            "h2o,tag1=foo,tag2=bar field1=70.6,field4=true 1000",
            "h2o,tag1=foo,tag2=bar field1=70.3,field5=false 3000",
        ];

        all_scenarios_for_one_chunk(vec![], vec![], lp_lines, "h2o", partition_key).await
    }
}

#[derive(Debug)]
pub struct OneMeasurementManyFieldsWithDelete {}
#[async_trait]
impl DbSetup for OneMeasurementManyFieldsWithDelete {
    async fn make(&self) -> Vec<DbScenario> {
        let partition_key = "1970-01-01T00";

        // Order this so field3 comes before field2
        // (and thus the columns need to get reordered)
        let lp_lines = vec![
            "h2o,tag1=foo,tag2=bar field1=70.6,field3=2 100",
            "h2o,tag1=foo,tag2=bar field1=70.4,field2=\"ss\" 100",
            "h2o,tag1=foo,tag2=bar field1=70.5,field2=\"ss\" 100",
            "h2o,tag1=foo,tag2=bar field1=70.6,field4=true 1000",
            "h2o,tag1=foo,tag2=bar field1=70.3,field5=false 3000",
        ];

        // pred: delete from h2o where 1000 <= time <= 1100
        // 1 rows of h2o with timestamp 1000 will be deleted which means
        // field4 no longer available
        let delete_table_name = "h2o";
        let pred = DeletePredicate {
            range: TimestampRange::new(1000, 1100),
            exprs: vec![],
        };

        all_scenarios_for_one_chunk(
            vec![&pred],
            vec![],
            lp_lines,
            delete_table_name,
            partition_key,
        )
        .await
    }
}

/// This data (from end to end test)
#[derive(Debug)]
pub struct EndToEndTest {}
#[async_trait]
impl DbSetup for EndToEndTest {
    async fn make(&self) -> Vec<DbScenario> {
        let lp_lines = vec![
            "cpu_load_short,host=server01,region=us-west value=0.64 0000",
            "cpu_load_short,host=server01 value=27.99 1000",
            "cpu_load_short,host=server02,region=us-west value=3.89 2000",
            "cpu_load_short,host=server01,region=us-east value=1234567.891011 3000",
            "cpu_load_short,host=server01,region=us-west value=0.000003 4000",
            "system,host=server03 uptime=1303385 5000",
            "swap,host=server01,name=disk0 in=3,out=4 6000",
            "status active=t 7000",
            "attributes color=\"blue\" 8000",
        ];

        let partition_key = "1970-01-01T00";
        // return all possible scenarios a chunk: MUB open, MUB frozen, RUB, RUB & OS, OS
        all_scenarios_for_one_chunk(vec![], vec![], lp_lines, "cpu_load_short", partition_key).await
    }
}

#[derive(Debug)]
pub struct EndToEndTestWithDelete {}
#[async_trait]
impl DbSetup for EndToEndTestWithDelete {
    async fn make(&self) -> Vec<DbScenario> {
        let lp_lines = vec![
            "cpu_load_short,host=server01,region=us-west value=0.64 0000",
            "cpu_load_short,host=server01 value=27.99 1000",
            "cpu_load_short,host=server02,region=us-west value=3.89 2000",
            "cpu_load_short,host=server01,region=us-east value=1234567.891011 3000",
            "cpu_load_short,host=server01,region=us-west value=0.000003 4000",
            "system,host=server03 uptime=1303385 5000",
            "swap,host=server01,name=disk0 in=3,out=4 6000",
            "status active=t 7000",
            "attributes color=\"blue\" 8000",
        ];

        let partition_key = "1970-01-01T00";

        // pred: delete from swap where 6000 <= time <= 6000 and name=disk0
        // 1 rows of h2o with timestamp 250 will be deleted
        let delete_table_name = "swap";
        let pred = DeletePredicate {
            range: TimestampRange::new(6000, 6000),
            exprs: vec![DeleteExpr::new(
                "name".to_string(),
                data_types::delete_predicate::Op::Eq,
                data_types::delete_predicate::Scalar::String(("disk0").to_string()),
            )],
        };

        all_scenarios_for_one_chunk(
            vec![&pred],
            vec![],
            lp_lines,
            delete_table_name,
            partition_key,
        )
        .await
    }
}

/// This creates two chunks but then drops them all. This should keep the tables.
#[derive(Debug)]
pub struct OneMeasurementAllChunksDropped {}
#[async_trait]
impl DbSetup for OneMeasurementAllChunksDropped {
    async fn make(&self) -> Vec<DbScenario> {
        let db = make_db().await.db;

        let partition_key = "1970-01-01T00";
        let table_name = "h2o";

        let lp_lines = vec!["h2o,state=MA temp=70.4 50"];
        write_lp(&db, &lp_lines.join("\n"));
        let chunk_id = db
            .compact_open_chunk(table_name, partition_key)
            .await
            .unwrap()
            .unwrap()
            .id();
        db.drop_chunk(table_name, partition_key, chunk_id)
            .await
            .unwrap();

        vec![DbScenario {
            scenario_name: "one measurement but all chunks are dropped".into(),
            db,
        }]
    }
}

#[derive(Debug)]
pub struct TwoMeasurementsMultiSeries {}
#[async_trait]
impl DbSetup for TwoMeasurementsMultiSeries {
    async fn make(&self) -> Vec<DbScenario> {
        let partition_key = "1970-01-01T00";

        let mut lp_lines = vec![
            "h2o,state=MA,city=Boston temp=70.4 100", // to row 2
            "h2o,state=MA,city=Boston temp=72.4 250", // to row 1
            "h2o,state=CA,city=LA temp=90.0 200",     // to row 0
            "h2o,state=CA,city=LA temp=90.0 350",     // to row 3
            "o2,state=MA,city=Boston temp=50.4,reading=50 100", // to row 5
            "o2,state=MA,city=Boston temp=53.4,reading=51 250", // to row 4
        ];

        // Swap around  data is not inserted in series order
        lp_lines.swap(0, 2);
        lp_lines.swap(4, 5);

        all_scenarios_for_one_chunk(vec![], vec![], lp_lines, "h2o", partition_key).await
    }
}

#[derive(Debug)]
pub struct TwoMeasurementsMultiSeriesWithDelete {}
#[async_trait]
impl DbSetup for TwoMeasurementsMultiSeriesWithDelete {
    async fn make(&self) -> Vec<DbScenario> {
        let partition_key = "1970-01-01T00";

        let mut lp_lines = vec![
            "h2o,state=MA,city=Boston temp=70.4 100", // to row 2
            "h2o,state=MA,city=Boston temp=72.4 250", // to row 1
            "h2o,state=CA,city=LA temp=90.0 200",     // to row 0
            "h2o,state=CA,city=LA temp=90.0 350",     // to row 3
            "o2,state=MA,city=Boston temp=50.4,reading=50 100", // to row 5
            "o2,state=MA,city=Boston temp=53.4,reading=51 250", // to row 4
        ];

        // Swap around  data is not inserted in series order
        lp_lines.swap(0, 2);
        lp_lines.swap(4, 5);

        // pred: delete from h2o where 120 <= time <= 250
        // 2 rows of h2o with timestamp 200 and 350 will be deleted
        let delete_table_name = "h2o";
        let pred = DeletePredicate {
            range: TimestampRange::new(120, 250),
            exprs: vec![],
        };

        all_scenarios_for_one_chunk(
            vec![&pred],
            vec![],
            lp_lines,
            delete_table_name,
            partition_key,
        )
        .await
    }
}

#[derive(Debug)]
pub struct TwoMeasurementsMultiSeriesWithDeleteAll {}
#[async_trait]
impl DbSetup for TwoMeasurementsMultiSeriesWithDeleteAll {
    async fn make(&self) -> Vec<DbScenario> {
        let partition_key = "1970-01-01T00";

        let mut lp_lines = vec![
            "h2o,state=MA,city=Boston temp=70.4 100", // to row 2
            "h2o,state=MA,city=Boston temp=72.4 250", // to row 1
            "h2o,state=CA,city=LA temp=90.0 200",     // to row 0
            "h2o,state=CA,city=LA temp=90.0 350",     // to row 3
            "o2,state=MA,city=Boston temp=50.4,reading=50 100", // to row 5
            "o2,state=MA,city=Boston temp=53.4,reading=51 250", // to row 4
        ];

        // Swap around  data is not inserted in series order
        lp_lines.swap(0, 2);
        lp_lines.swap(4, 5);

        // Delete all data form h2o
        // pred: delete from h20 where 100 <= time <= 360
        let delete_table_name = "h2o";
        let pred = DeletePredicate {
            range: TimestampRange::new(100, 360),
            exprs: vec![],
        };

        all_scenarios_for_one_chunk(
            vec![&pred],
            vec![],
            lp_lines,
            delete_table_name,
            partition_key,
        )
        .await
    }
}

pub struct MeasurementStatusCode {}
#[async_trait]
impl DbSetup for MeasurementStatusCode {
    async fn make(&self) -> Vec<DbScenario> {
        let partition_key = "2018-05-22T19";

        let lp = vec![
            "status_code,url=http://www.example.com value=404 1527018806000000000",
            "status_code,url=https://influxdb.com value=418 1527018816000000000",
        ];

        all_scenarios_for_one_chunk(vec![], vec![], lp, "status_code", partition_key).await
    }
}

#[derive(Debug)]
pub struct MeasurementsSortableTags {}
#[async_trait]
impl DbSetup for MeasurementsSortableTags {
    async fn make(&self) -> Vec<DbScenario> {
        let partition_key = "1970-01-01T00";

        let lp_lines = vec![
            "h2o,zz_tag=A,state=MA,city=Kingston temp=70.1 800",
            "h2o,state=MA,city=Kingston,zz_tag=B temp=70.2 100",
            "h2o,state=CA,city=Boston temp=70.3 250",
            "h2o,state=MA,city=Boston,zz_tag=A temp=70.4 1000",
            "h2o,state=MA,city=Boston temp=70.5,other=5.0 250",
        ];

        all_scenarios_for_one_chunk(vec![], vec![], lp_lines, "h2o", partition_key).await
    }
}

#[derive(Debug)]
pub struct MeasurementsSortableTagsWithDelete {}
#[async_trait]
impl DbSetup for MeasurementsSortableTagsWithDelete {
    async fn make(&self) -> Vec<DbScenario> {
        let partition_key = "1970-01-01T00";

        let lp_lines = vec![
            "h2o,zz_tag=A,state=MA,city=Kingston temp=70.1 800",
            "h2o,state=MA,city=Kingston,zz_tag=B temp=70.2 100",
            "h2o,state=CA,city=Boston temp=70.3 250", // soft deleted
            "h2o,state=MA,city=Boston,zz_tag=A temp=70.4 1000",
            "h2o,state=MA,city=Boston temp=70.5,other=5.0 250",
        ];

        // pred: delete from h2o where 120 <= time <= 350 and state=CA
        // 1 rows of h2o with timestamp 250 will be deleted
        let delete_table_name = "h2o";
        let pred = DeletePredicate {
            range: TimestampRange::new(120, 350),
            exprs: vec![DeleteExpr::new(
                "state".to_string(),
                data_types::delete_predicate::Op::Eq,
                data_types::delete_predicate::Scalar::String(("CA").to_string()),
            )],
        };

        all_scenarios_for_one_chunk(
            vec![&pred],
            vec![],
            lp_lines,
            delete_table_name,
            partition_key,
        )
        .await
    }
}

// See issue: https://github.com/influxdata/influxdb_iox/issues/2845
#[derive(Debug)]
pub struct MeasurementsForDefect2845 {}
#[async_trait]
impl DbSetup for MeasurementsForDefect2845 {
    async fn make(&self) -> Vec<DbScenario> {
        let partition_key = "2018-05-22T19";

        let lp_lines = vec![
            "system,host=host.local load1=1.83 1527018806000000000",
            "system,host=host.local load1=1.63 1527018816000000000",
            "system,host=host.local load3=1.72 1527018806000000000",
            "system,host=host.local load4=1.77 1527018806000000000",
            "system,host=host.local load4=1.78 1527018816000000000",
            "system,host=host.local load4=1.77 1527018826000000000",
        ];

        all_scenarios_for_one_chunk(vec![], vec![], lp_lines, "system", partition_key).await
    }
}

pub struct OneMeasurementNoTags2 {}
#[async_trait]
impl DbSetup for OneMeasurementNoTags2 {
    async fn make(&self) -> Vec<DbScenario> {
        let partition_key = "1970-01-01T00";
        let lp_lines = vec!["m0 foo=1.0 1", "m0 foo=2.0 2"];
        all_scenarios_for_one_chunk(vec![], vec![], lp_lines, "m0", partition_key).await
    }
}

pub struct OneMeasurementNoTagsWithDelete {}
#[async_trait]
impl DbSetup for OneMeasurementNoTagsWithDelete {
    async fn make(&self) -> Vec<DbScenario> {
        let partition_key = "1970-01-01T00";
        let lp_lines = vec!["m0 foo=1.0 1", "m0 foo=2.0 2"];

        // pred: delete from m0 where 1 <= time <= 1 and foo=1.0
        // 1 row of m0 with timestamp 1
        let delete_table_name = "m0";
        let pred = DeletePredicate {
            range: TimestampRange::new(1, 1),
            exprs: vec![DeleteExpr::new(
                "foo".to_string(),
                data_types::delete_predicate::Op::Eq,
                data_types::delete_predicate::Scalar::F64((1.0).into()),
            )],
        };

        all_scenarios_for_one_chunk(
            vec![&pred],
            vec![],
            lp_lines,
            delete_table_name,
            partition_key,
        )
        .await
    }
}

/// This will create many scenarios (at least 15), some have a chunk with
/// soft deleted data, some have no chunks because there is no point to
/// create a RUB for one or many compacted MUB with all deleted data.
pub struct OneMeasurementNoTagsWithDeleteAllWithAndWithoutChunk {}
#[async_trait]
impl DbSetup for OneMeasurementNoTagsWithDeleteAllWithAndWithoutChunk {
    async fn make(&self) -> Vec<DbScenario> {
        let partition_key = "1970-01-01T00";
        let lp_lines = vec!["m0 foo=1.0 1", "m0 foo=2.0 2"];

        // pred: delete from m0 where 1 <= time <= 2
        let delete_table_name = "m0";
        let pred = DeletePredicate {
            range: TimestampRange::new(1, 2),
            exprs: vec![],
        };

        // Apply predicate before the chunk is moved if any. There will be
        // scenario without chunks as a consequence of not-compacting-deleted-data
        all_scenarios_for_one_chunk(
            vec![&pred],
            vec![],
            lp_lines,
            delete_table_name,
            partition_key,
        )
        .await
    }
}

pub struct OneMeasurementForAggs {}
#[async_trait]
impl DbSetup for OneMeasurementForAggs {
    async fn make(&self) -> Vec<DbScenario> {
        let partition_key = "1970-01-01T00";

        let lp_lines1 = vec![
            "h2o,state=MA,city=Boston temp=70.4 100",
            "h2o,state=MA,city=Boston temp=72.4 250",
        ];
        let lp_lines2 = vec![
            "h2o,state=CA,city=LA temp=90.0 200",
            "h2o,state=CA,city=LA temp=90.0 350",
        ];

        make_two_chunk_scenarios(partition_key, &lp_lines1.join("\n"), &lp_lines2.join("\n")).await
    }
}

pub struct AnotherMeasurementForAggs {}
#[async_trait]
impl DbSetup for AnotherMeasurementForAggs {
    async fn make(&self) -> Vec<DbScenario> {
        let partition_key = "1970-01-01T00";

        let lp_lines1 = vec![
            "h2o,state=MA,city=Cambridge temp=80 50",
            "h2o,state=MA,city=Cambridge temp=81 100",
            "h2o,state=MA,city=Cambridge temp=82 200",
            "h2o,state=MA,city=Boston temp=70 300",
        ];
        let lp_lines2 = vec![
            "h2o,state=MA,city=Boston temp=71 400",
            "h2o,state=CA,city=LA temp=90,humidity=10 500",
            "h2o,state=CA,city=LA temp=91,humidity=11 600",
        ];

        make_two_chunk_scenarios(partition_key, &lp_lines1.join("\n"), &lp_lines2.join("\n")).await
    }
}

pub struct TwoMeasurementForAggs {}
#[async_trait]
impl DbSetup for TwoMeasurementForAggs {
    async fn make(&self) -> Vec<DbScenario> {
        let partition_key = "1970-01-01T00";

        let lp_lines1 = vec![
            "h2o,state=MA,city=Boston temp=70.4 100",
            "h2o,state=MA,city=Boston temp=72.4 250",
        ];
        let lp_lines2 = vec![
            "o2,state=CA,city=LA temp=90.0 200",
            "o2,state=CA,city=LA temp=90.0 350",
        ];

        make_two_chunk_scenarios(partition_key, &lp_lines1.join("\n"), &lp_lines2.join("\n")).await
    }
}

pub struct MeasurementForSelectors {}
#[async_trait]
impl DbSetup for MeasurementForSelectors {
    async fn make(&self) -> Vec<DbScenario> {
        let partition_key = "1970-01-01T00";

        let lp_lines1 = vec!["h2o,state=MA,city=Cambridge f=8.0,i=8i,b=true,s=\"d\" 1000"];
        let lp_lines2 = vec![
            "h2o,state=MA,city=Cambridge f=7.0,i=7i,b=true,s=\"c\" 2000",
            "h2o,state=MA,city=Cambridge f=6.0,i=6i,b=false,s=\"b\" 3000",
            "h2o,state=MA,city=Cambridge f=5.0,i=5i,b=false,s=\"a\" 4000",
        ];

        make_two_chunk_scenarios(partition_key, &lp_lines1.join("\n"), &lp_lines2.join("\n")).await
    }
}

pub struct MeasurementForMin {}
#[async_trait]
impl DbSetup for MeasurementForMin {
    async fn make(&self) -> Vec<DbScenario> {
        let partition_key = "1970-01-01T00";

        let lp_lines1 = vec![
            "h2o,state=MA,city=Cambridge f=8.0,i=8i,b=false,s=\"c\" 1000",
            "h2o,state=MA,city=Cambridge f=7.0,i=7i,b=true,s=\"a\" 2000",
        ];
        let lp_lines2 = vec![
            "h2o,state=MA,city=Cambridge f=6.0,i=6i,b=true,s=\"z\" 3000",
            "h2o,state=MA,city=Cambridge f=5.0,i=5i,b=false,s=\"c\" 4000",
        ];

        make_two_chunk_scenarios(partition_key, &lp_lines1.join("\n"), &lp_lines2.join("\n")).await
    }
}

pub struct MeasurementForMax {}
#[async_trait]
impl DbSetup for MeasurementForMax {
    async fn make(&self) -> Vec<DbScenario> {
        let partition_key = "1970-01-01T00";

        let lp_lines1 = vec![
            "h2o,state=MA,city=Cambridge f=8.0,i=8i,b=true,s=\"c\" 1000",
            "h2o,state=MA,city=Cambridge f=7.0,i=7i,b=false,s=\"d\" 2000",
            "h2o,state=MA,city=Cambridge f=6.0,i=6i,b=true,s=\"a\" 3000",
        ];
        let lp_lines2 = vec!["h2o,state=MA,city=Cambridge f=5.0,i=5i,b=false,s=\"z\" 4000"];

        make_two_chunk_scenarios(partition_key, &lp_lines1.join("\n"), &lp_lines2.join("\n")).await
    }
}

pub struct MeasurementForGroupKeys {}
#[async_trait]
impl DbSetup for MeasurementForGroupKeys {
    async fn make(&self) -> Vec<DbScenario> {
        let partition_key = "1970-01-01T00";

        let lp_lines1 = vec![
            "h2o,state=MA,city=Cambridge temp=80 50",
            "h2o,state=MA,city=Cambridge temp=81 100",
            "h2o,state=MA,city=Cambridge temp=82 200",
        ];
        let lp_lines2 = vec![
            "h2o,state=MA,city=Boston temp=70 300",
            "h2o,state=MA,city=Boston temp=71 400",
            "h2o,state=CA,city=LA temp=90,humidity=10 500",
            "h2o,state=CA,city=LA temp=91,humidity=11 600",
        ];

        make_two_chunk_scenarios(partition_key, &lp_lines1.join("\n"), &lp_lines2.join("\n")).await
    }
}

pub struct MeasurementForGroupByField {}
#[async_trait]
impl DbSetup for MeasurementForGroupByField {
    async fn make(&self) -> Vec<DbScenario> {
        let partition_key = "1970-01-01T00";

        let lp_lines1 = vec![
            "system,host=local,region=A load1=1.1,load2=2.1 100",
            "system,host=local,region=A load1=1.2,load2=2.2 200",
            "system,host=remote,region=B load1=10.1,load2=2.1 100",
        ];

        let lp_lines2 = vec![
            "system,host=remote,region=B load1=10.2,load2=20.2 200",
            "system,host=local,region=C load1=100.1,load2=200.1 100",
            "aa_system,host=local,region=C load1=100.1,load2=200.1 100",
        ];

        make_two_chunk_scenarios(partition_key, &lp_lines1.join("\n"), &lp_lines2.join("\n")).await
    }
}

// Test data to validate fix for:
// https://github.com/influxdata/influxdb_iox/issues/2691
pub struct MeasurementForDefect2691 {}
#[async_trait]
impl DbSetup for MeasurementForDefect2691 {
    async fn make(&self) -> Vec<DbScenario> {
        let partition_key = "2018-05-22T19";

        let lp = vec![
            "system,host=host.local load1=1.83 1527018806000000000",
            "system,host=host.local load1=1.63 1527018816000000000",
            "system,host=host.local load3=1.72 1527018806000000000",
            "system,host=host.local load4=1.77 1527018806000000000",
            "system,host=host.local load4=1.78 1527018816000000000",
            "system,host=host.local load4=1.77 1527018826000000000",
        ];

        all_scenarios_for_one_chunk(vec![], vec![], lp, "system", partition_key).await
    }
}

pub struct MeasurementForWindowAggregate {}
#[async_trait]
impl DbSetup for MeasurementForWindowAggregate {
    async fn make(&self) -> Vec<DbScenario> {
        let partition_key = "1970-01-01T00";

        let lp_lines1 = vec![
            "h2o,state=MA,city=Boston temp=70.0 100",
            "h2o,state=MA,city=Boston temp=71.0 200",
            "h2o,state=MA,city=Boston temp=72.0 300",
            "h2o,state=MA,city=Boston temp=73.0 400",
            "h2o,state=MA,city=Boston temp=74.0 500",
            "h2o,state=MA,city=Cambridge temp=80.0 100",
            "h2o,state=MA,city=Cambridge temp=81.0 200",
        ];
        let lp_lines2 = vec![
            "h2o,state=MA,city=Cambridge temp=82.0 300",
            "h2o,state=MA,city=Cambridge temp=83.0 400",
            "h2o,state=MA,city=Cambridge temp=84.0 500",
            "h2o,state=CA,city=LA temp=90.0 100",
            "h2o,state=CA,city=LA temp=91.0 200",
            "h2o,state=CA,city=LA temp=92.0 300",
            "h2o,state=CA,city=LA temp=93.0 400",
            "h2o,state=CA,city=LA temp=94.0 500",
        ];

        make_two_chunk_scenarios(partition_key, &lp_lines1.join("\n"), &lp_lines2.join("\n")).await
    }
}

pub struct MeasurementForWindowAggregateMonths {}
#[async_trait]
impl DbSetup for MeasurementForWindowAggregateMonths {
    async fn make(&self) -> Vec<DbScenario> {
        // Note the lines are written into 4 different partititions (as we are
        // partitioned by day, effectively)
        let lp_lines = vec![
            "h2o,state=MA,city=Boston temp=70.0 1583020800000000000", // 2020-03-01T00:00:00Z
            "h2o,state=MA,city=Boston temp=71.0 1583107920000000000", // 2020-03-02T00:12:00Z
            "h2o,state=MA,city=Boston temp=72.0 1585699200000000000", // 2020-04-01T00:00:00Z
            "h2o,state=MA,city=Boston temp=73.0 1585785600000000000", // 2020-04-02T00:00:00Z
        ];
        // partition keys are: ["2020-03-02T00", "2020-03-01T00", "2020-04-01T00",
        // "2020-04-02T00"]

        let db = make_db().await.db;
        let data = lp_lines.join("\n");
        write_lp(&db, &data);
        let scenario1 = DbScenario {
            scenario_name: "Data in 4 partitions, open chunks of mutable buffer".into(),
            db,
        };

        let db = make_db().await.db;
        let data = lp_lines.join("\n");
        write_lp(&db, &data);
        db.rollover_partition("h2o", "2020-03-01T00").await.unwrap();
        db.rollover_partition("h2o", "2020-03-02T00").await.unwrap();
        let scenario2 = DbScenario {
            scenario_name:
                "Data in 4 partitions, two open chunk and two closed chunks of mutable buffer"
                    .into(),
            db,
        };

        let db = make_db().await.db;
        let data = lp_lines.join("\n");
        write_lp(&db, &data);
        // roll over and load chunks into both RUB and OS
        rollover_and_load(&db, "2020-03-01T00", "h2o").await;
        rollover_and_load(&db, "2020-03-02T00", "h2o").await;
        rollover_and_load(&db, "2020-04-01T00", "h2o").await;
        rollover_and_load(&db, "2020-04-02T00", "h2o").await;
        let scenario3 = DbScenario {
            scenario_name: "Data in 4 partitions, 4 closed chunks in mutable buffer".into(),
            db,
        };

        // TODO: Add a scenario for OS only in #1342

        vec![scenario1, scenario2, scenario3]
    }
}

// Test data to validate fix for:
// https://github.com/influxdata/influxdb_iox/issues/2697
pub struct MeasurementForDefect2697 {}
#[async_trait]
impl DbSetup for MeasurementForDefect2697 {
    async fn make(&self) -> Vec<DbScenario> {
        let partition_key = "2021-01-01T00";

        let lp = vec![
            "mm,section=1a bar=5.0 1609459201000000011",
            "mm,section=1a bar=0.28 1609459201000000031",
            "mm,section=2b bar=4.0 1609459201000000009",
            "mm,section=2b bar=6.0 1609459201000000015",
            "mm,section=2b bar=1.2 1609459201000000022",
            "mm,section=1a foo=1.0 1609459201000000001",
            "mm,section=1a foo=3.0 1609459201000000005",
            "mm,section=1a foo=11.24 1609459201000000024",
            "mm,section=2b foo=2.0 1609459201000000002",
        ];

        all_scenarios_for_one_chunk(vec![], vec![], lp, "mm", partition_key).await
    }
}

pub struct MeasurementForDefect2697WithDelete {}
#[async_trait]
impl DbSetup for MeasurementForDefect2697WithDelete {
    async fn make(&self) -> Vec<DbScenario> {
        let partition_key = "2021-01-01T00";

        let lp = vec![
            "mm,section=1a bar=5.0 1609459201000000011",
            "mm,section=1a bar=0.28 1609459201000000031",
            "mm,section=2b bar=4.0 1609459201000000009",
            "mm,section=2b bar=6.0 1609459201000000015",
            "mm,section=2b bar=1.2 1609459201000000022",
            "mm,section=1a foo=1.0 1609459201000000001",
            "mm,section=1a foo=3.0 1609459201000000005",
            "mm,section=1a foo=11.24 1609459201000000024",
            "mm,section=2b foo=2.0 1609459201000000002",
        ];

        // pred: delete from mm where 1609459201000000022 <= time <= 1609459201000000022
        // 1 row of m0 with timestamp 1609459201000000022 (section=2b bar=1.2)
        let delete_table_name = "mm";
        let pred = DeletePredicate {
            range: TimestampRange::new(1609459201000000022, 1609459201000000022),
            exprs: vec![],
        };

        all_scenarios_for_one_chunk(vec![&pred], vec![], lp, delete_table_name, partition_key).await
    }
}

pub struct MeasurementForDefect2697WithDeleteAll {}
#[async_trait]
impl DbSetup for MeasurementForDefect2697WithDeleteAll {
    async fn make(&self) -> Vec<DbScenario> {
        let partition_key = "2021-01-01T00";

        let lp = vec![
            "mm,section=1a bar=5.0 1609459201000000011",
            "mm,section=1a bar=0.28 1609459201000000031",
            "mm,section=2b bar=4.0 1609459201000000009",
            "mm,section=2b bar=6.0 1609459201000000015",
            "mm,section=2b bar=1.2 1609459201000000022",
            "mm,section=1a foo=1.0 1609459201000000001",
            "mm,section=1a foo=3.0 1609459201000000005",
            "mm,section=1a foo=11.24 1609459201000000024",
            "mm,section=2b foo=2.0 1609459201000000002",
        ];

        // pred: delete from mm where 1 <= time <= 1609459201000000031
        let delete_table_name = "mm";
        let pred = DeletePredicate {
            range: TimestampRange::new(1, 1609459201000000031),
            exprs: vec![],
        };

        all_scenarios_for_one_chunk(vec![&pred], vec![], lp, delete_table_name, partition_key).await
    }
}

// Test data to validate fix for:
// https://github.com/influxdata/influxdb_iox/issues/2890
pub struct MeasurementForDefect2890 {}
#[async_trait]
impl DbSetup for MeasurementForDefect2890 {
    async fn make(&self) -> Vec<DbScenario> {
        let partition_key = "2021-01-01T00";

        let lp = vec![
            "mm foo=2.0 1609459201000000001",
            "mm foo=2.0 1609459201000000002",
            "mm foo=3.0 1609459201000000005",
            "mm foo=11.24 1609459201000000024",
            "mm bar=4.0 1609459201000000009",
            "mm bar=5.0 1609459201000000011",
            "mm bar=6.0 1609459201000000015",
            "mm bar=1.2 1609459201000000022",
            "mm bar=2.8 1609459201000000031",
        ];

        all_scenarios_for_one_chunk(vec![], vec![], lp, "mm", partition_key).await
    }
}

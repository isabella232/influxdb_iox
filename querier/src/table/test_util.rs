use std::sync::Arc;

use iox_catalog::interface::get_schema_by_name;
use iox_tests::util::{TestCatalog, TestTable};
use schema::Schema;

use crate::{cache::CatalogCache, chunk::ParquetChunkAdapter};

use super::QuerierTable;

pub async fn querier_table(catalog: &Arc<TestCatalog>, table: &Arc<TestTable>) -> QuerierTable {
    let catalog_cache = Arc::new(CatalogCache::new(
        catalog.catalog(),
        catalog.time_provider(),
    ));
    let chunk_adapter = Arc::new(ParquetChunkAdapter::new(
        catalog_cache,
        catalog.object_store(),
        catalog.metric_registry(),
        catalog.time_provider(),
    ));

    let mut repos = catalog.catalog.repositories().await;
    let mut catalog_schema = get_schema_by_name(&table.namespace.namespace.name, repos.as_mut())
        .await
        .unwrap();
    let schema = catalog_schema.tables.remove(&table.table.name).unwrap();
    let schema = Arc::new(Schema::try_from(schema).unwrap());

    QuerierTable::new(
        table.table.id,
        table.table.name.clone().into(),
        schema,
        chunk_adapter,
    )
}

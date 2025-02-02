use std::sync::Arc;

use crate::cache::CatalogCache;
use iox_tests::util::{TestCatalog, TestNamespace};

use super::QuerierNamespace;

pub fn querier_namespace(catalog: &Arc<TestCatalog>, ns: &Arc<TestNamespace>) -> QuerierNamespace {
    QuerierNamespace::new(
        Arc::new(CatalogCache::new(
            catalog.catalog(),
            catalog.time_provider(),
        )),
        ns.namespace.name.clone().into(),
        ns.namespace.id,
        catalog.metric_registry(),
        catalog.object_store(),
        catalog.time_provider(),
        catalog.exec(),
    )
}

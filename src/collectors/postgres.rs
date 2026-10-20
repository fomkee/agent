use async_trait::async_trait;
use sqlx::{PgPool, postgres::PgPoolOptions};

use crate::{
    collector::{Collection, Collector, CollectorError},
    model::{CollectorId, Measurement, MeasurementKind, Unit},
};

pub struct PostgresCollector {
    id: CollectorId,
    pool: PgPool,
}

impl std::fmt::Debug for PostgresCollector {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PostgresCollector")
            .field("id", &self.id)
            .field("pool", &"[REDACTED]")
            .finish()
    }
}

impl PostgresCollector {
    pub fn new(id: CollectorId, database_url: &str) -> Result<Self, CollectorError> {
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .connect_lazy(database_url)
            .map_err(|_| {
                CollectorError::new(
                    "postgres_config",
                    "database URL from the configured environment variable is invalid",
                )
            })?;
        Ok(Self { id, pool })
    }
}

#[async_trait]
impl Collector for PostgresCollector {
    fn id(&self) -> &CollectorId {
        &self.id
    }

    async fn collect(&self) -> Result<Collection, CollectorError> {
        let (database_size, connections, temp_files, temp_bytes, deadlocks) =
            sqlx::query_as::<_, (i64, i64, i64, i64, i64)>(
                "SELECT pg_database_size(current_database())::bigint AS database_size_bytes, \
             numbackends::bigint AS connections, temp_files::bigint AS temp_files, \
             temp_bytes::bigint AS temp_bytes, deadlocks::bigint AS deadlocks \
             FROM pg_stat_database WHERE datname = current_database()",
            )
            .fetch_one(&self.pool)
            .await
            .map_err(|error| CollectorError::new("postgres_query", error.to_string()))?;
        Ok(Collection::new(
            vec![
                gauge("database_size", Unit::Bytes, database_size)?,
                gauge("connections", Unit::Count, connections)?,
                counter("temp_files_total", Unit::Count, temp_files)?,
                counter("temp_bytes_total", Unit::Bytes, temp_bytes)?,
                counter("deadlocks_total", Unit::Count, deadlocks)?,
            ],
            Vec::new(),
        ))
    }
}

fn gauge(name: &str, unit: Unit, value: i64) -> Result<Measurement, CollectorError> {
    numeric(name, MeasurementKind::Gauge, unit, value)
}

fn counter(name: &str, unit: Unit, value: i64) -> Result<Measurement, CollectorError> {
    numeric(name, MeasurementKind::Counter, unit, value)
}

fn numeric(
    name: &str,
    kind: MeasurementKind,
    unit: Unit,
    value: i64,
) -> Result<Measurement, CollectorError> {
    if value < 0 {
        return Err(CollectorError::new(
            "postgres_value",
            format!("{name} unexpectedly returned a negative value"),
        ));
    }
    Measurement::scalar(name, kind, unit, value as f64)
        .map_err(|error| CollectorError::new("invalid_measurement", error.to_string()))
}

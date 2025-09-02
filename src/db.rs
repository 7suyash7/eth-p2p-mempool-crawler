use anyhow::Result;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use tracing::info;

pub async fn create_pool(database_url: &str) -> Result<PgPool> {
    info!(target: "crawler::db", "Connecting to database...");

    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(database_url)
        .await?;

    info!(target: "crawler::db", "Database connection pool successfully created");
    Ok(pool)
}

use sqlx::SqlitePool;

mod models;
pub use models::*;

/// Initialize the database: run migrations and return pool.
pub async fn init(database_url: &str) -> anyhow::Result<SqlitePool> {
    let pool = SqlitePool::connect(database_url).await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS workflow_runs (
            id TEXT PRIMARY KEY,
            workflow_name TEXT NOT NULL,
            workflow_version TEXT NOT NULL,
            yaml_content TEXT NOT NULL,
            status TEXT NOT NULL DEFAULT 'pending',
            node_count INTEGER NOT NULL DEFAULT 0,
            started_at TEXT,
            finished_at TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            error_message TEXT
        )",
    )
    .execute(&pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS node_runs (
            id TEXT PRIMARY KEY,
            run_id TEXT NOT NULL REFERENCES workflow_runs(id),
            node_id TEXT NOT NULL,
            node_type TEXT NOT NULL,
            status TEXT NOT NULL DEFAULT 'pending',
            attempt INTEGER NOT NULL DEFAULT 0,
            started_at TEXT,
            finished_at TEXT,
            exit_code INTEGER,
            stdout TEXT,
            stderr TEXT,
            error_message TEXT
        )",
    )
    .execute(&pool)
    .await?;

    // Add outputs column for node output persistence (idempotent)
    let _ = sqlx::query("ALTER TABLE node_runs ADD COLUMN outputs TEXT")
        .execute(&pool)
        .await;

    // Add depends column for storing expanded dependency info (idempotent)
    let _ = sqlx::query("ALTER TABLE node_runs ADD COLUMN depends TEXT")
        .execute(&pool)
        .await;

    // Enable foreign keys (SQLite has them off by default)
    sqlx::query("PRAGMA foreign_keys = ON")
        .execute(&pool)
        .await?;

    // Add indexes for common query patterns
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_workflow_runs_created_at ON workflow_runs(created_at DESC)",
    )
    .execute(&pool)
    .await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_node_runs_run_id ON node_runs(run_id)")
        .execute(&pool)
        .await?;

    tracing::info!("database initialized");
    Ok(pool)
}

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{Pool, Sqlite, sqlite::SqlitePoolOptions};
use sqlx::Row;
use tracing::info;

#[derive(Clone)]
pub struct Storage {
    pool: Pool<Sqlite>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Worker {
    pub id: i64,
    pub name: String,
    pub authorized: bool,
    pub last_seen: Option<DateTime<Utc>>,
}

impl Storage {
    pub async fn connect(path: &str) -> Result<Self> {
        let url = format!("sqlite://{path}");
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect(&url)
            .await?;
        Ok(Self { pool })
    }

    pub async fn ensure_schema(&self) -> Result<()> {
        // Minimal schema: workers, shares, blocks
        sqlx::query(r#"
            CREATE TABLE IF NOT EXISTS workers(
              id INTEGER PRIMARY KEY AUTOINCREMENT,
              name TEXT UNIQUE NOT NULL,
              authorized INTEGER NOT NULL DEFAULT 0,
              last_seen TEXT
            );
        "#).execute(&self.pool).await?;

        sqlx::query(r#"
            CREATE TABLE IF NOT EXISTS shares(
              id INTEGER PRIMARY KEY AUTOINCREMENT,
              worker_id INTEGER NOT NULL,
              timestamp TEXT NOT NULL,
              difficulty REAL NOT NULL,
              valid INTEGER NOT NULL,
              FOREIGN KEY(worker_id) REFERENCES workers(id)
            );
        "#).execute(&self.pool).await?;

        sqlx::query(r#"
            CREATE TABLE IF NOT EXISTS blocks(
              id INTEGER PRIMARY KEY AUTOINCREMENT,
              height INTEGER,
              hash TEXT,
              status TEXT, -- found|orphan|confirmed
              timestamp TEXT NOT NULL
            );
        "#).execute(&self.pool).await?;

        info!("DB schema ensured");
        Ok(())
    }

    pub async fn upsert_worker(&self, name: &str, authorized: bool) -> Result<i64> {
        sqlx::query(r#"
            INSERT INTO workers(name, authorized, last_seen)
            VALUES(?1, ?2, ?3)
            ON CONFLICT(name) DO UPDATE SET last_seen=excluded.last_seen
        "#)
        .bind(name)
        .bind(authorized as i64)
        .bind(chrono::Utc::now().to_rfc3339())
        .execute(&self.pool)
        .await?;

        let rec: (i64,) = sqlx::query_as("SELECT id FROM workers WHERE name=?1")
            .bind(name)
            .fetch_one(&self.pool)
            .await?;
        Ok(rec.0)
    }

    pub async fn list_workers(&self) -> Result<Vec<Worker>> {
        // Use the runtime query API (no macros), so no DATABASE_URL or offline prepare is required
        let rows = sqlx::query(
            r#"
            SELECT id, name, authorized, last_seen
            FROM workers
            ORDER BY name
            "#
        )
        .fetch_all(&self.pool)
        .await?;

        let out = rows
            .into_iter()
            .map(|r| {
                let id: i64 = r.get("id");
                let name: String = r.get("name");
                let authorized_i: i64 = r.get("authorized");
                let last_seen_str: Option<String> = r.get("last_seen");
                let last_seen = last_seen_str
                    .and_then(|s| s.parse::<chrono::DateTime<chrono::Utc>>().ok());

                Worker {
                    id,
                    name,
                    authorized: authorized_i != 0,
                    last_seen,
                }
            })
            .collect();
        Ok(out)
    }

    pub async fn insert_share(&self, worker_id: i64, difficulty: f64, valid: bool) -> Result<()> {
        sqlx::query(r#"
            INSERT INTO shares(worker_id, timestamp, difficulty, valid)
            VALUES (?1, ?2, ?3, ?4)
        "#)
        .bind(worker_id)
        .bind(chrono::Utc::now().to_rfc3339())
        .bind(difficulty)
        .bind(valid as i64)
        .execute(&self.pool).await?;
        Ok(())
    }

    pub async fn recent_blocks(&self, limit: i64) -> Result<Vec<(Option<i64>, Option<String>, String, String)>> {
        let rows = sqlx::query_as::<_, (Option<i64>, Option<String>, String, String)>(r#"
            SELECT height, hash, status, timestamp FROM blocks ORDER BY id DESC LIMIT ?1
        "#).bind(limit).fetch_all(&self.pool).await?;
        Ok(rows)
    }

    pub async fn record_block(&self, height: Option<i64>, hash: &str, status: &str) -> Result<()> {
        sqlx::query(r#"
            INSERT INTO blocks(height, hash, status, timestamp) VALUES(?1, ?2, ?3, ?4)
        "#)
        .bind(height)
        .bind(hash)
        .bind(status)
        .bind(chrono::Utc::now().to_rfc3339())
        .execute(&self.pool).await?;
        Ok(())
    }
}

impl std::ops::Deref for Storage {
    type Target = Pool<Sqlite>;
    fn deref(&self) -> &Self::Target { &self.pool }
}
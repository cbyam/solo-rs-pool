use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqliteSynchronous};
use sqlx::{sqlite::SqlitePoolOptions, Pool, Row, Sqlite};
use std::path::Path;
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
        if let Some(parent) = Path::new(path).parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }

        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal);

        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await?;

        Ok(Self { pool })
    }

    pub async fn ensure_schema(&self) -> Result<()> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS workers(
              id INTEGER PRIMARY KEY AUTOINCREMENT,
              name TEXT UNIQUE NOT NULL,
              authorized INTEGER NOT NULL DEFAULT 0,
              last_seen TEXT
            );
            CREATE TABLE IF NOT EXISTS shares(
              id INTEGER PRIMARY KEY AUTOINCREMENT,
              worker_id INTEGER NOT NULL,
              timestamp TEXT NOT NULL,
              difficulty REAL NOT NULL,
              valid INTEGER NOT NULL,
              FOREIGN KEY(worker_id) REFERENCES workers(id)
            );
            CREATE TABLE IF NOT EXISTS blocks(
              id INTEGER PRIMARY KEY AUTOINCREMENT,
              height INTEGER,
              hash TEXT,
              status TEXT,
              timestamp TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS worker_shares (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                worker TEXT NOT NULL,
                ts INTEGER NOT NULL,           -- unix seconds
                share_diff REAL NOT NULL       -- pool share difficulty; Stage-A uses 1.0
            );
            CREATE INDEX IF NOT EXISTS idx_worker_shares_worker_ts
                ON worker_shares(worker, ts);
            "#,
        )
        .execute(&self.pool)
        .await?;
        info!("DB schema ensured");
        Ok(())
    }

    pub async fn upsert_worker(&self, name: &str, authorized: bool) -> Result<i64> {
        sqlx::query(
            r#"
            INSERT INTO workers(name, authorized, last_seen)
            VALUES (?1, ?2, ?3)
            ON CONFLICT(name) DO UPDATE SET last_seen = excluded.last_seen
            "#,
        )
        .bind(name)
        .bind(authorized as i64)
        .bind(chrono::Utc::now().to_rfc3339())
        .execute(&self.pool)
        .await?;

        let row: (i64,) = sqlx::query_as("SELECT id FROM workers WHERE name = ?1")
            .bind(name)
            .fetch_one(&self.pool)
            .await?;
        Ok(row.0)
    }

    pub async fn list_workers(&self) -> Result<Vec<Worker>> {
        let rows = sqlx::query(
            r#"
            SELECT id, name, authorized, last_seen
            FROM workers
            ORDER BY name
            "#,
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|r| {
                let id: i64 = r.get("id");
                let name: String = r.get("name");
                let authorized_i: i64 = r.get("authorized");
                let last_seen_str: Option<String> = r.get("last_seen");
                let last_seen = last_seen_str.and_then(|s| s.parse::<DateTime<Utc>>().ok());

                Worker {
                    id,
                    name,
                    authorized: authorized_i != 0,
                    last_seen,
                }
            })
            .collect())
    }

    pub async fn insert_share(&self, worker_id: i64, difficulty: f64, valid: bool) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO shares(worker_id, timestamp, difficulty, valid)
            VALUES (?1, ?2, ?3, ?4)
            "#,
        )
        .bind(worker_id)
        .bind(chrono::Utc::now().to_rfc3339())
        .bind(difficulty)
        .bind(valid as i64)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn recent_blocks(
        &self,
        limit: i64,
    ) -> Result<Vec<(Option<i64>, Option<String>, String, String)>> {
        sqlx::query_as(
            r#"
            SELECT height, hash, status, timestamp
            FROM blocks
            ORDER BY id DESC
            LIMIT ?1
            "#,
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(anyhow::Error::from)
    }

    pub async fn record_block(&self, height: Option<i64>, hash: &str, status: &str) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO blocks(height, hash, status, timestamp)
            VALUES (?1, ?2, ?3, ?4)
            "#,
        )
        .bind(height)
        .bind(hash)
        .bind(status)
        .bind(chrono::Utc::now().to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Record an accepted share for a worker (Stage-A: share_diff = 1.0).
    pub async fn record_share(&self, worker: &str, share_diff: f64) -> Result<()> {
        let ts = Utc::now().timestamp();
        sqlx::query("INSERT INTO worker_shares(worker, ts, share_diff) VALUES (?1, ?2, ?3)")
            .bind(worker)
            .bind(ts)
            .bind(share_diff)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Returns (count, sum_diff) for `worker` over the last `window_secs`.
    pub async fn share_window(&self, worker: &str, window_secs: i64) -> Result<(i64, f64)> {
        let cutoff = Utc::now().timestamp() - window_secs;
        let (count, sum): (i64, Option<f64>) = sqlx::query_as(
            "SELECT COUNT(*), SUM(share_diff)
             FROM worker_shares
             WHERE worker = ?1 AND ts >= ?2",
        )
        .bind(worker)
        .bind(cutoff)
        .fetch_one(&self.pool)
        .await?;
        Ok((count, sum.unwrap_or(0.0)))
    }

    /// Per-minute buckets of share counts for the past `minutes`.
    /// Returns Vec<(bucket_unix_ts, count)>
    pub async fn share_buckets(&self, worker: &str, minutes: i64) -> Result<Vec<(i64, i64)>> {
        let cutoff = Utc::now().timestamp() - minutes * 60;
        let rows: Vec<(i64, i64)> = sqlx::query_as(
            "SELECT (ts/60)*60 AS bucket, COUNT(*)
             FROM worker_shares
             WHERE worker = ?1 AND ts >= ?2
             GROUP BY bucket
             ORDER BY bucket",
        )
        .bind(worker)
        .bind(cutoff)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// Rolling hashrate estimate (H/s) over `window_secs`:
    ///   H ≈ Σ(share_diff) * 2^32 / window_secs
    pub async fn worker_hashrate_hps(&self, worker: &str, window_secs: i64) -> Result<f64> {
        if window_secs <= 0 {
            return Ok(0.0);
        }
        let (_count, sum_diff) = self.share_window(worker, window_secs).await?;
        const TWO32: f64 = 4294967296.0; // 2^32
        Ok(sum_diff * TWO32 / (window_secs as f64))
    }
}

impl std::ops::Deref for Storage {
    type Target = Pool<Sqlite>;
    fn deref(&self) -> &Self::Target {
        &self.pool
    }
}

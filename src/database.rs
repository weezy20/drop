use chrono::{DateTime, Utc};
use color_eyre::eyre::{Context, Result};
use sqlx::{PgPool, Row};
use std::path::PathBuf;
use tracing::{info, warn};
use uuid::Uuid;

#[derive(Clone, Debug, sqlx::FromRow)]
pub struct FileMapping {
    pub id: Uuid,
    pub filename: String,
    pub content_type: String,
    pub file_path: Option<String>,
    pub file_size: i64,
    pub is_in_memory: bool,
    pub created_at: DateTime<Utc>,
    pub accessed_at: DateTime<Utc>,
    pub access_count: i32,
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, sqlx::FromRow)]
pub struct ShortUrl {
    pub short_code: String,
    pub file_id: Uuid,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, sqlx::FromRow)]
pub struct RateLimit {
    pub client_ip: String,
    pub request_count: i32,
    pub window_start: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone)]
pub struct Database {
    pool: PgPool,
}

impl Database {
    pub async fn new(database_url: &str) -> Result<Self> {
        info!("Connecting to database: {}", database_url.replace(
            &database_url.split('@').collect::<Vec<&str>>()[0].split("://").collect::<Vec<&str>>()[1],
            "***"
        ));
        
        let pool = PgPool::connect(database_url)
            .await
            .with_context(|| format!("Failed to connect to database: {}", database_url))?;

        // Run migrations
        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .context("Failed to run database migrations")?;

        info!("Database connected and migrations applied successfully");
        Ok(Self { pool })
    }

    pub async fn health_check(&self) -> bool {
        match sqlx::query("SELECT 1")
            .fetch_one(&self.pool)
            .await
        {
            Ok(_) => true,
            Err(e) => {
                warn!("Database health check failed: {}", e);
                false
            }
        }
    }

    pub async fn store_file_mapping(
        &self,
        id: Uuid,
        filename: &str,
        content_type: &str,
        file_path: Option<&PathBuf>,
        file_size: i64,
        is_in_memory: bool,
        expires_at: Option<DateTime<Utc>>,
    ) -> Result<()> {
        let file_path_str = file_path.map(|p| p.to_string_lossy().to_string());
        
        let query = r#"
            INSERT INTO file_mappings (id, filename, content_type, file_path, file_size, is_in_memory, expires_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7)
        "#;

        sqlx::query(query)
            .bind(id)
            .bind(filename)
            .bind(content_type)
            .bind(file_path_str)
            .bind(file_size)
            .bind(is_in_memory)
            .bind(expires_at)
            .execute(&self.pool)
            .await
            .with_context(|| format!("Failed to store file mapping for ID: {}", id))?;

        Ok(())
    }

    pub async fn get_file_mapping(&self, id: Uuid) -> Result<Option<FileMapping>> {
        let query = r#"
            UPDATE file_mappings 
            SET accessed_at = NOW(), access_count = access_count + 1
            WHERE id = $1
            RETURNING *
        "#;

        let result = sqlx::query_as::<_, FileMapping>(query)
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .with_context(|| format!("Failed to get file mapping for ID: {}", id))?;

        Ok(result)
    }

    pub async fn store_short_url(&self, short_code: &str, file_id: Uuid) -> Result<()> {
        let query = r#"
            INSERT INTO short_urls (short_code, file_id)
            VALUES ($1, $2)
            ON CONFLICT (short_code) DO UPDATE SET file_id = $2
        "#;

        sqlx::query(query)
            .bind(short_code)
            .bind(file_id)
            .execute(&self.pool)
            .await
            .with_context(|| format!("Failed to store short URL: {}", short_code))?;

        Ok(())
    }

    pub async fn get_file_id_by_short_code(&self, short_code: &str) -> Result<Option<Uuid>> {
        let query = "SELECT file_id FROM short_urls WHERE short_code = $1";

        let result = sqlx::query(query)
            .bind(short_code)
            .fetch_optional(&self.pool)
            .await
            .with_context(|| format!("Failed to get file ID for short code: {}", short_code))?;

        Ok(result.map(|row| row.get("file_id")))
    }

    pub async fn check_rate_limit(
        &self,
        client_ip: std::net::IpAddr,
        window_seconds: u64,
        max_requests: i32,
    ) -> Result<bool> {
        let client_ip_str = client_ip.to_string();
        let now = Utc::now();
        let window_start = now - chrono::Duration::seconds(window_seconds as i64);

        // First, try to get existing rate limit record
        let query = r#"
            SELECT request_count, window_start
            FROM rate_limits
            WHERE client_ip = $1 AND window_start > $2
        "#;

        let existing = sqlx::query(query)
            .bind(&client_ip_str)
            .bind(window_start)
            .fetch_optional(&self.pool)
            .await
            .context("Failed to check existing rate limit")?;

        match existing {
            Some(row) => {
                let request_count: i32 = row.get("request_count");
                if request_count >= max_requests {
                    return Ok(false); // Rate limit exceeded
                }

                // Update existing record
                let update_query = r#"
                    UPDATE rate_limits
                    SET request_count = request_count + 1, updated_at = NOW()
                    WHERE client_ip = $1
                "#;

                sqlx::query(update_query)
                    .bind(&client_ip_str)
                    .execute(&self.pool)
                    .await
                    .context("Failed to update rate limit")?;
            }
            None => {
                // Create new record or reset if outside window
                let upsert_query = r#"
                    INSERT INTO rate_limits (client_ip, request_count, window_start)
                    VALUES ($1, 1, $2)
                    ON CONFLICT (client_ip)
                    DO UPDATE SET 
                        request_count = 1,
                        window_start = $2,
                        updated_at = NOW()
                "#;

                sqlx::query(upsert_query)
                    .bind(&client_ip_str)
                    .bind(now)
                    .execute(&self.pool)
                    .await
                    .context("Failed to create rate limit record")?;
            }
        }

        Ok(true) // Rate limit not exceeded
    }

    pub async fn cleanup_expired_files(&self) -> Result<Vec<Uuid>> {
        let query = r#"
            DELETE FROM file_mappings
            WHERE expires_at IS NOT NULL AND expires_at < NOW()
            RETURNING id
        "#;

        let results = sqlx::query(query)
            .fetch_all(&self.pool)
            .await
            .context("Failed to cleanup expired files")?;

        let expired_ids: Vec<Uuid> = results.into_iter()
            .map(|row| row.get("id"))
            .collect();

        if !expired_ids.is_empty() {
            info!("Cleaned up {} expired files", expired_ids.len());
        }

        Ok(expired_ids)
    }

    pub async fn cleanup_old_rate_limits(&self) -> Result<i64> {
        let cutoff = Utc::now() - chrono::Duration::minutes(10); // Keep rate limits for 10 minutes

        let query = "DELETE FROM rate_limits WHERE updated_at < $1";

        let result = sqlx::query(query)
            .bind(cutoff)
            .execute(&self.pool)
            .await
            .context("Failed to cleanup old rate limits")?;

        let deleted_count = result.rows_affected() as i64;
        if deleted_count > 0 {
            info!("Cleaned up {} old rate limit records", deleted_count);
        }

        Ok(deleted_count)
    }

    pub async fn get_storage_stats(&self) -> Result<(i64, i64, i64)> {
        let query = r#"
            SELECT 
                COUNT(*) as total_files,
                COALESCE(SUM(file_size)::BIGINT, 0) as total_size,
                COUNT(*) FILTER (WHERE is_in_memory = true) as memory_files
            FROM file_mappings
        "#;

        let row = sqlx::query(query)
            .fetch_one(&self.pool)
            .await
            .context("Failed to get storage stats")?;

        let total_files: i64 = row.get("total_files");
        let total_size: i64 = row.get("total_size");
        let memory_files: i64 = row.get("memory_files");

        Ok((total_files, total_size, memory_files))
    }
}
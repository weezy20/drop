use axum::{
    Router,
    body::Body,
    extract::{Multipart, Path, State, ConnectInfo},
    http::{StatusCode, header},
    response::{IntoResponse, Json},
    routing::{get, post},
};
use color_eyre::eyre::Result;
use sanitize_filename::sanitize;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::env;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};
use sysinfo::System;
use tokio::io::AsyncWriteExt;
use tokio_util::io::ReaderStream;
use tracing::{error, info, instrument, warn};
use uuid::Uuid;
use xxhash_rust::xxh3::Xxh3;

pub mod database;
use database::Database;

// Fallback in-memory storage for when database is down
pub type FileStorage = Arc<Mutex<HashMap<String, FileData>>>;
// URL shortener mapping: short_code -> full_uuid (fallback)
pub type ShortUrlStorage = Arc<Mutex<HashMap<String, String>>>;
// Rate limiting: IP -> (last_request_time, request_count) (fallback)
pub type RateLimitStorage = Arc<Mutex<HashMap<String, (Instant, u32)>>>;

#[derive(Clone, Debug)]
pub struct Config {
    pub min_file_size_limit: usize,
    pub max_file_size_limit: usize,
    pub max_total_size_per_request: usize,
    pub stream_threshold: usize,
    pub temp_directory: PathBuf,
    pub bind_address: String,
    pub memory_pool_ratio: f64,
    pub reserved_memory_mb: usize,
    pub rate_limit_requests_per_minute: u32,
    pub rate_limit_window_seconds: u64,
    pub database_url: Option<String>,
    pub redis_url: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            min_file_size_limit: 50 * 1024 * 1024,               // 50MB
            max_file_size_limit: 5 * 1024 * 1024 * 1024,         // 5GB
            max_total_size_per_request: 10 * 1024 * 1024 * 1024, // 10GB
            stream_threshold: 50 * 1024 * 1024,                  // 50MB
            temp_directory: PathBuf::from("./temp"),
            bind_address: "0.0.0.0:3000".to_string(),
            memory_pool_ratio: 0.5,
            reserved_memory_mb: 200,
            rate_limit_requests_per_minute: 60,
            rate_limit_window_seconds: 60,
            database_url: None,
            redis_url: None,
        }
    }
}

impl Config {
    pub fn from_env() -> Self {
        let mut config = Self::default();

        if let Ok(val) = env::var("DROP_MIN_FILE_SIZE_MB") {
            if let Ok(size) = val.parse::<usize>() {
                config.min_file_size_limit = size * 1024 * 1024;
            }
        }

        if let Ok(val) = env::var("DROP_MAX_FILE_SIZE_GB") {
            if let Ok(size) = val.parse::<usize>() {
                config.max_file_size_limit = size * 1024 * 1024 * 1024;
            }
        }

        if let Ok(val) = env::var("DROP_MAX_TOTAL_SIZE_GB") {
            if let Ok(size) = val.parse::<usize>() {
                config.max_total_size_per_request = size * 1024 * 1024 * 1024;
            }
        }

        if let Ok(val) = env::var("DROP_STREAM_THRESHOLD_MB") {
            if let Ok(size) = val.parse::<usize>() {
                config.stream_threshold = size * 1024 * 1024;
            }
        }

        if let Ok(val) = env::var("DROP_TEMP_DIR") {
            config.temp_directory = PathBuf::from(val);
        }

        if let Ok(val) = env::var("DROP_BIND_ADDRESS") {
            config.bind_address = val;
        }

        if let Ok(val) = env::var("DROP_MEMORY_POOL_RATIO") {
            if let Ok(ratio) = val.parse::<f64>() {
                if ratio > 0.0 && ratio <= 1.0 {
                    config.memory_pool_ratio = ratio;
                }
            }
        }

        if let Ok(val) = env::var("DROP_RATE_LIMIT_RPM") {
            if let Ok(rpm) = val.parse::<u32>() {
                config.rate_limit_requests_per_minute = rpm;
            }
        }

        // Database configuration
        config.database_url = env::var("DATABASE_URL").ok();
        config.redis_url = env::var("REDIS_URL").ok();

        config
    }
}

// Application state
#[derive(Clone)]
pub struct AppState {
    pub file_storage: FileStorage,       // Fallback in-memory storage
    pub short_url_storage: ShortUrlStorage, // Fallback short URL storage
    pub rate_limit_storage: RateLimitStorage, // Fallback rate limiting
    pub config: Config,
    pub database: Option<Database>,      // Primary database (PostgreSQL)
    pub database_healthy: Arc<std::sync::atomic::AtomicBool>, // Database health status
}

// Memory pool for tracking allocated memory
static MEMORY_POOL: AtomicUsize = AtomicUsize::new(0);
static ALLOCATED_MEMORY: AtomicUsize = AtomicUsize::new(0);
static ACTIVE_CONNECTIONS: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileData {
    pub filename: String,
    pub content_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Vec<u8>>, // In-memory data
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_path: Option<PathBuf>, // Disk-based path
}

#[derive(Serialize)]
pub struct UploadResponse {
    id: String,
    short_url: String,
    full_url: String,
}

#[derive(Serialize)]
pub struct HealthResponse {
    status: String,
    database: String,
    memory_pool: String,
    active_connections: usize,
    storage_stats: Option<StorageStats>,
}

#[derive(Serialize)]
pub struct StorageStats {
    total_files: i64,
    total_size: i64,
    memory_files: i64,
    memory_usage_mb: usize,
    pool_size_mb: usize,
}

pub fn initialize_memory_pool() {
    let mut system = System::new_all();
    system.refresh_memory();

    let total_memory = system.total_memory();
    let available_memory = system.available_memory();

    // Reserve 200MB for system and other processes, use 50% of remaining available memory
    let reserved_memory = 200 * 1024 * 1024; // 200MB
    let pool_size = if available_memory > reserved_memory {
        ((available_memory - reserved_memory) as f64 * 0.5) as usize
    } else {
        100 * 1024 * 1024 // Fallback to 100MB if low memory
    };

    MEMORY_POOL.store(pool_size, Ordering::Relaxed);

    info!(
        "System memory: total={} MB, available={} MB",
        total_memory / (1024 * 1024),
        available_memory / (1024 * 1024)
    );
    info!(
        "Initialized memory pool with {} MB for file storage",
        pool_size / (1024 * 1024)
    );
}

fn try_allocate_memory(size: usize) -> bool {
    let current_allocated = ALLOCATED_MEMORY.load(Ordering::Acquire);
    let pool_size = MEMORY_POOL.load(Ordering::Acquire);

    if current_allocated + size <= pool_size {
        // Try to atomically increment the allocated memory
        let old_value = ALLOCATED_MEMORY.fetch_add(size, Ordering::AcqRel);

        // Double-check after allocation to handle race conditions
        if old_value + size <= pool_size {
            info!(
                "Allocated {} bytes from memory pool ({}MB/{}MB used)",
                size,
                (old_value + size) / (1024 * 1024),
                pool_size / (1024 * 1024)
            );
            true
        } else {
            // Rollback allocation if we exceeded the pool
            ALLOCATED_MEMORY.fetch_sub(size, Ordering::AcqRel);
            warn!(
                "Memory allocation failed: would exceed pool limit ({}MB available)",
                (pool_size - old_value) / (1024 * 1024)
            );
            false
        }
    } else {
        warn!(
            "Memory allocation failed: {} bytes requested, only {} bytes available in pool",
            size,
            pool_size.saturating_sub(current_allocated)
        );
        false
    }
}

#[allow(dead_code)]
fn deallocate_memory(size: usize) {
    let old_value = ALLOCATED_MEMORY.fetch_sub(size, Ordering::AcqRel);
    info!(
        "Deallocated {} bytes from memory pool ({}MB/{}MB used)",
        size,
        old_value.saturating_sub(size) / (1024 * 1024),
        MEMORY_POOL.load(Ordering::Acquire) / (1024 * 1024)
    );
}

fn generate_short_code() -> String {
    use std::hash::{Hash, Hasher};

    // Use current timestamp + random UUID to generate unique short code
    let uuid = Uuid::new_v4();
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_else(|_| {
            warn!("System time is before Unix epoch, using fallback timestamp");
            std::time::Duration::from_nanos(0)
        })
        .as_nanos();

    // Use XXH3 for superior speed and distribution properties
    let mut hasher = Xxh3::new();
    uuid.hash(&mut hasher);
    timestamp.hash(&mut hasher);

    // Add some extra entropy from process-specific data
    std::process::id().hash(&mut hasher);
    std::thread::current().id().hash(&mut hasher);

    let hash = hasher.finish();

    // Simple base36 encoding using alphanumeric characters
    let chars = "0123456789abcdefghijklmnopqrstuvwxyz";
    let mut result = String::new();
    let mut n = hash;

    for _ in 0..8 {
        let idx = (n % 36) as usize;
        result.push(chars.chars().nth(idx).unwrap()); // This unwrap is safe - idx is always 0-35
        n /= 36;
    }

    result
}

async fn resolve_id_or_short_code_db(
    input: &str,
    app_state: &AppState,
) -> Option<Uuid> {
    // First try to parse as UUID
    if let Ok(uuid) = input.parse::<Uuid>() {
        return Some(uuid);
    }

    // Otherwise, try to resolve as short code - database first
    if let Some(ref db) = app_state.database {
        if app_state.database_healthy.load(std::sync::atomic::Ordering::Relaxed) {
            match db.get_file_id_by_short_code(input).await {
                Ok(Some(file_id)) => return Some(file_id),
                Ok(None) => {}, // Not found in database, try memory
                Err(e) => {
                    warn!("Database short code lookup failed: {}", e);
                    app_state.database_healthy.store(false, std::sync::atomic::Ordering::Relaxed);
                }
            }
        }
    }

    // Fallback to in-memory storage
    if let Ok(storage_guard) = app_state.short_url_storage.lock() {
        if let Some(full_id) = storage_guard.get(input) {
            if let Ok(uuid) = full_id.parse::<Uuid>() {
                return Some(uuid);
            }
        }
    } else {
        error!("Failed to acquire lock on short URL storage");
    }

    None
}

// Health check endpoint
#[instrument(skip(app_state))]
pub async fn health_check(State(app_state): State<AppState>) -> impl IntoResponse {
    let database_status = if let Some(ref db) = app_state.database {
        if db.health_check().await {
            app_state.database_healthy.store(true, std::sync::atomic::Ordering::Relaxed);
            "healthy".to_string()
        } else {
            app_state.database_healthy.store(false, std::sync::atomic::Ordering::Relaxed);
            "unhealthy".to_string()
        }
    } else {
        "not_configured".to_string()
    };

    let storage_stats = if let Some(ref db) = app_state.database {
        if let Ok((total_files, total_size, memory_files)) = db.get_storage_stats().await {
            Some(StorageStats {
                total_files,
                total_size,
                memory_files,
                memory_usage_mb: ALLOCATED_MEMORY.load(Ordering::Acquire) / (1024 * 1024),
                pool_size_mb: MEMORY_POOL.load(Ordering::Acquire) / (1024 * 1024),
            })
        } else {
            None
        }
    } else {
        // Fallback stats from in-memory storage
        let file_count = if let Ok(storage) = app_state.file_storage.lock() {
            storage.len() as i64
        } else {
            0
        };

        Some(StorageStats {
            total_files: file_count,
            total_size: 0, // We don't track this in memory storage
            memory_files: file_count,
            memory_usage_mb: ALLOCATED_MEMORY.load(Ordering::Acquire) / (1024 * 1024),
            pool_size_mb: MEMORY_POOL.load(Ordering::Acquire) / (1024 * 1024),
        })
    };

    let overall_status = if database_status == "healthy" || database_status == "not_configured" {
        "healthy"
    } else {
        "degraded" // Database is down but we can fall back to in-memory
    };

    let response = HealthResponse {
        status: overall_status.to_string(),
        database: database_status,
        memory_pool: format!(
            "{} MB / {} MB", 
            ALLOCATED_MEMORY.load(Ordering::Acquire) / (1024 * 1024),
            MEMORY_POOL.load(Ordering::Acquire) / (1024 * 1024)
        ),
        active_connections: ACTIVE_CONNECTIONS.load(Ordering::Acquire),
        storage_stats,
    };

    Json(response)
}

// Helper function to create temp directory and handle cleanup
async fn ensure_temp_directory(temp_dir: &PathBuf) -> Result<(), StatusCode> {
    if let Err(e) = tokio::fs::create_dir_all(temp_dir).await {
        error!("Failed to create temp directory: {:?}", e);
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }
    Ok(())
}

// Helper function to format file sizes for logging
fn format_size(size: usize) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB"];
    let mut size = size as f64;
    let mut unit_index = 0;

    while size >= 1024.0 && unit_index < UNITS.len() - 1 {
        size /= 1024.0;
        unit_index += 1;
    }

    format!("{:.2} {}", size, UNITS[unit_index])
}

// Security: Sanitize filename to prevent path traversal attacks
fn sanitize_filename(filename: &str) -> String {
    let sanitized = sanitize(filename);

    // Additional security checks
    if sanitized.is_empty() || sanitized == "." || sanitized == ".." {
        return "unknown_file".to_string();
    }

    // Limit filename length
    if sanitized.len() > 200 {
        return format!(
            "{}...{}",
            &sanitized[..100],
            &sanitized[sanitized.len() - 50..]
        );
    }

    sanitized
}

// Extract client IP from connection info or headers
fn get_client_ip(connect_info: Option<&ConnectInfo<SocketAddr>>) -> std::net::IpAddr {
    connect_info
        .map(|ci| ci.0.ip())
        .unwrap_or_else(|| "127.0.0.1".parse().unwrap())
}

// Rate limiting check - tries database first, falls back to in-memory
async fn check_rate_limit(
    client_ip: std::net::IpAddr,
    app_state: &AppState,
) -> Result<(), StatusCode> {
    // Try database first if available and healthy
    if let Some(ref db) = app_state.database {
        if app_state.database_healthy.load(std::sync::atomic::Ordering::Relaxed) {
            match db.check_rate_limit(
                client_ip,
                app_state.config.rate_limit_window_seconds,
                app_state.config.rate_limit_requests_per_minute as i32,
            ).await {
                Ok(allowed) => {
                    if !allowed {
                        warn!("Rate limit exceeded for IP: {}", client_ip);
                        return Err(StatusCode::TOO_MANY_REQUESTS);
                    }
                    return Ok(());
                }
                Err(e) => {
                    warn!("Database rate limit check failed, falling back to memory: {}", e);
                    app_state.database_healthy.store(false, std::sync::atomic::Ordering::Relaxed);
                }
            }
        }
    }

    // Fallback to in-memory rate limiting
    check_rate_limit_memory(&client_ip.to_string(), &app_state.rate_limit_storage, &app_state.config)
}

// In-memory rate limiting (fallback)
fn check_rate_limit_memory(
    client_ip: &str,
    rate_storage: &RateLimitStorage,
    config: &Config,
) -> Result<(), StatusCode> {
    let now = Instant::now();
    let window_duration = Duration::from_secs(config.rate_limit_window_seconds);

    if let Ok(mut storage) = rate_storage.lock() {
        let entry = storage.entry(client_ip.to_string()).or_insert((now, 0));

        // Reset counter if window has passed
        if now.duration_since(entry.0) > window_duration {
            entry.0 = now;
            entry.1 = 0;
        }

        entry.1 += 1;

        if entry.1 > config.rate_limit_requests_per_minute {
            warn!("Rate limit exceeded for IP: {}", client_ip);
            return Err(StatusCode::TOO_MANY_REQUESTS);
        }

        Ok(())
    } else {
        error!("Failed to acquire rate limit storage lock");
        Err(StatusCode::INTERNAL_SERVER_ERROR)
    }
}

// Helper function to stream large files directly to disk
async fn stream_field_to_disk(
    mut field: axum::extract::multipart::Field<'_>,
    file_path: &PathBuf,
    max_size: usize,
) -> Result<usize, StatusCode> {
    let mut file = tokio::fs::File::create(file_path).await.map_err(|e| {
        error!("Failed to create file for streaming: {:?}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let mut total_size = 0usize;
    let mut buffer = Vec::with_capacity(8192); // 8KB buffer

    while let Some(chunk) = field.chunk().await.map_err(|e| {
        error!("Failed to read chunk during streaming: {:?}", e);
        StatusCode::BAD_REQUEST
    })? {
        total_size += chunk.len();

        // Check size limit during streaming
        if total_size > max_size {
            // Clean up partial file
            let _ = tokio::fs::remove_file(file_path).await;
            return Err(StatusCode::PAYLOAD_TOO_LARGE);
        }

        buffer.extend_from_slice(&chunk);

        // Write in larger chunks for better performance
        if buffer.len() >= 8192 {
            file.write_all(&buffer).await.map_err(|e| {
                error!("Failed to write chunk to disk: {:?}", e);
                StatusCode::INTERNAL_SERVER_ERROR
            })?;
            buffer.clear();
        }
    }

    // Write remaining data
    if !buffer.is_empty() {
        file.write_all(&buffer).await.map_err(|e| {
            error!("Failed to write final chunk to disk: {:?}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    }

    file.flush().await.map_err(|e| {
        error!("Failed to flush file to disk: {:?}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    Ok(total_size)
}

#[instrument(skip(app_state, multipart))]
pub async fn upload_file(
    State(app_state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    mut multipart: Multipart,
) -> Result<Json<UploadResponse>, StatusCode> {
    info!("Starting file upload");

    // Rate limiting
    let client_ip = get_client_ip(Some(&ConnectInfo(addr)));
    check_rate_limit(client_ip, &app_state).await?;

    // Increment active connections
    ACTIVE_CONNECTIONS.fetch_add(1, Ordering::Relaxed);

    let mut total_size = 0usize;

    // Process the multipart form data
    while let Some(field) = multipart.next_field().await.map_err(|e| {
        error!("Failed to get next field: {:?}", e);
        ACTIVE_CONNECTIONS.fetch_sub(1, Ordering::Relaxed);
        StatusCode::BAD_REQUEST
    })? {
        let raw_filename = field.file_name().unwrap_or("unknown").to_string();
        let filename = sanitize_filename(&raw_filename);
        info!(
            "Processing file: {} (sanitized from: {})",
            filename, raw_filename
        );

        let content_type = field
            .content_type()
            .unwrap_or("application/octet-stream") // Standard fallback for binary data
            .to_string();

        // Generate a unique ID for the file early
        let id = Uuid::new_v4();
        let short_code = generate_short_code();
        info!("Generated file ID: {}, short code: {}", id, short_code);

        // Store the short URL mapping - try database first, fallback to memory
        let short_url_stored = if let Some(ref db) = app_state.database {
            if app_state.database_healthy.load(std::sync::atomic::Ordering::Relaxed) {
                match db.store_short_url(&short_code, id).await {
                    Ok(_) => {
                        info!("Stored short URL in database: {}", short_code);
                        true
                    }
                    Err(e) => {
                        warn!("Failed to store short URL in database, falling back to memory: {}", e);
                        app_state.database_healthy.store(false, std::sync::atomic::Ordering::Relaxed);
                        false
                    }
                }
            } else {
                false
            }
        } else {
            false
        };

        if !short_url_stored {
            // Fallback to in-memory storage
            if let Ok(mut storage_guard) = app_state.short_url_storage.lock() {
                storage_guard.insert(short_code.clone(), id.to_string());
                info!("Stored short URL in memory: {}", short_code);
            } else {
                error!("Failed to acquire lock on short URL storage during upload");
                ACTIVE_CONNECTIONS.fetch_sub(1, Ordering::Relaxed);
                return Err(StatusCode::INTERNAL_SERVER_ERROR);
            }
        }

        // Create temp directory if it doesn't exist
        ensure_temp_directory(&app_state.config.temp_directory).await?;

        // Always stream to disk first for large file support
        let file_path = app_state.config.temp_directory.join(format!("file_{}", id));
        let file_size =
            stream_field_to_disk(field, &file_path, app_state.config.max_file_size_limit).await?;

        // Check total request size limit
        total_size += file_size;
        if total_size > app_state.config.max_total_size_per_request {
            error!(
                "Total request size exceeds maximum limit of {}",
                format_size(app_state.config.max_total_size_per_request)
            );
            // Clean up the file we just wrote
            let _ = tokio::fs::remove_file(&file_path).await;
            ACTIVE_CONNECTIONS.fetch_sub(1, Ordering::Relaxed);
            return Err(StatusCode::PAYLOAD_TOO_LARGE);
        }

        info!(
            "File size: {}, content_type: {}, total_request_size: {}",
            format_size(file_size),
            content_type,
            format_size(total_size)
        );

        // Decide whether to keep in memory or on disk based on size and memory availability
        let file_data =
            if file_size < app_state.config.stream_threshold && try_allocate_memory(file_size) {
                info!(
                    "Moving file '{}' to memory pool (size: {})",
                    filename,
                    format_size(file_size)
                );

                // Read file into memory and delete from disk
                match tokio::fs::read(&file_path).await {
                    Ok(data) => {
                        // Delete the temporary file since we have it in memory
                        if let Err(e) = tokio::fs::remove_file(&file_path).await {
                            warn!("Failed to remove temporary file: {:?}", e);
                        }

                        FileData {
                            filename: filename.clone(),
                            content_type: content_type.clone(),
                            data: Some(data),
                            file_path: None,
                        }
                    }
                    Err(e) => {
                        error!("Failed to read file into memory: {:?}", e);
                        deallocate_memory(file_size);
                        FileData {
                            filename: filename.clone(),
                            content_type: content_type.clone(),
                            data: None,
                            file_path: Some(file_path),
                        }
                    }
                }
            } else {
                info!(
                    "Keeping file '{}' on disk (size: {})",
                    filename,
                    format_size(file_size)
                );
                FileData {
                    filename: filename.clone(),
                    content_type: content_type.clone(),
                    data: None,
                    file_path: Some(file_path),
                }
            };

        // Store file mapping - try database first, fallback to memory
        let file_stored = if let Some(ref db) = app_state.database {
            if app_state.database_healthy.load(std::sync::atomic::Ordering::Relaxed) {
                let is_in_memory = file_data.data.is_some();
                let file_path_for_db = if is_in_memory { None } else { file_data.file_path.as_ref() };
                
                match db.store_file_mapping(
                    id,
                    &filename,
                    &content_type,
                    file_path_for_db,
                    file_size as i64,
                    is_in_memory,
                    None, // No expiration for now
                ).await {
                    Ok(_) => {
                        info!("Stored file mapping in database: {}", id);
                        true
                    }
                    Err(e) => {
                        warn!("Failed to store file mapping in database, falling back to memory: {}", e);
                        app_state.database_healthy.store(false, std::sync::atomic::Ordering::Relaxed);
                        false
                    }
                }
            } else {
                false
            }
        } else {
            false
        };

        if !file_stored {
            // Fallback to in-memory storage
            if let Ok(mut storage_guard) = app_state.file_storage.lock() {
                storage_guard.insert(id.to_string(), file_data);
                info!("Successfully stored file '{}' with ID: {}", filename, id);
            } else {
                error!("Failed to acquire lock on file storage during upload");
                ACTIVE_CONNECTIONS.fetch_sub(1, Ordering::Relaxed);
                return Err(StatusCode::INTERNAL_SERVER_ERROR);
            }
        }

        // Decrement active connections
        ACTIVE_CONNECTIONS.fetch_sub(1, Ordering::Relaxed);

        // Return the ID and short URL
        return Ok(Json(UploadResponse {
            id: id.to_string(),
            short_url: format!(
                "http://{}/drop/{}",
                app_state.config.bind_address, short_code
            ),
            full_url: format!("http://{}/drop/{}", app_state.config.bind_address, id),
        }));
    }

    // Decrement active connections if no files found
    ACTIVE_CONNECTIONS.fetch_sub(1, Ordering::Relaxed);
    warn!("No files found in multipart request");
    Err(StatusCode::BAD_REQUEST)
}

#[instrument(skip(app_state))]
pub async fn download_file(
    Path(id): Path<String>,
    State(app_state): State<AppState>,
) -> impl IntoResponse {
    info!("Attempting to download file with ID: {}", id);

    // Resolve short code to full UUID if needed
    let resolved_id = resolve_id_or_short_code_db(&id, &app_state).await;

    if let Some(uuid) = resolved_id {
        info!("Resolved ID: {}", uuid);

        // Try to get file from database first
        if let Some(ref db) = app_state.database {
            if app_state.database_healthy.load(std::sync::atomic::Ordering::Relaxed) {
                match db.get_file_mapping(uuid).await {
                    Ok(Some(file_mapping)) => {
                        let headers = [
                            (header::CONTENT_TYPE, file_mapping.content_type.clone()),
                            (
                                header::CONTENT_DISPOSITION,
                                format!("attachment; filename=\"{}\"", file_mapping.filename),
                            ),
                        ];

                        // Return data based on storage type
                        if file_mapping.is_in_memory {
                            // Try to get from in-memory storage
                            if let Ok(storage_guard) = app_state.file_storage.lock() {
                                if let Some(file_data) = storage_guard.get(&uuid.to_string()) {
                                    if let Some(ref data) = file_data.data {
                                        info!(
                                            "Successfully serving file '{}' from memory, size: {} bytes",
                                            file_mapping.filename,
                                            data.len()
                                        );
                                        return (headers, data.clone()).into_response();
                                    }
                                }
                            }
                            // If not in memory, fall through to file system
                        }

                        // Serve from file system
                        if let Some(file_path_str) = file_mapping.file_path {
                            let file_path = PathBuf::from(file_path_str);
                            match tokio::fs::File::open(&file_path).await {
                                Ok(file) => {
                                    let stream = ReaderStream::new(file);
                                    let body = Body::from_stream(stream);

                                    info!("Streaming file '{}' from disk", file_mapping.filename);
                                    return (headers, body).into_response();
                                }
                                Err(e) => {
                                    error!("Failed to open file from disk: {:?}", e);
                                    return StatusCode::INTERNAL_SERVER_ERROR.into_response();
                                }
                            }
                        }
                    }
                    Ok(None) => {
                        // File not found in database, try fallback
                    }
                    Err(e) => {
                        warn!("Database file lookup failed, falling back to memory: {}", e);
                        app_state.database_healthy.store(false, std::sync::atomic::Ordering::Relaxed);
                    }
                }
            }
        }

        // Fallback to in-memory storage
        let file_data = {
            match app_state.file_storage.lock() {
                Ok(storage_guard) => storage_guard.get(&uuid.to_string()).cloned(),
                Err(e) => {
                    error!(
                        "Failed to acquire lock on file storage during download: {}",
                        e
                    );
                    return StatusCode::INTERNAL_SERVER_ERROR.into_response();
                }
            }
        };

        if let Some(file_data) = file_data {
            let headers = [
                (header::CONTENT_TYPE, file_data.content_type.clone()),
                (
                    header::CONTENT_DISPOSITION,
                    format!("attachment; filename=\"{}\"", file_data.filename),
                ),
            ];

            // Return data based on storage type
            match (&file_data.data, &file_data.file_path) {
                (Some(data), None) => {
                    info!(
                        "Successfully serving file '{}' from memory, size: {} bytes",
                        file_data.filename,
                        data.len()
                    );
                    (headers, data.clone()).into_response()
                }
                (None, Some(path)) => {
                    info!(
                        "Successfully serving file '{}' from disk with streaming",
                        file_data.filename
                    );

                    // Use streaming for better memory efficiency with large files
                    match tokio::fs::File::open(path).await {
                        Ok(file) => {
                            let stream = ReaderStream::new(file);
                            let body = Body::from_stream(stream);

                            info!("Streaming file '{}' from disk", file_data.filename);
                            (headers, body).into_response()
                        }
                        Err(e) => {
                            error!("Failed to open file from disk: {:?}", e);
                            StatusCode::INTERNAL_SERVER_ERROR.into_response()
                        }
                    }
                }
                _ => {
                    error!("Invalid file data state for ID: {}", uuid);
                    StatusCode::INTERNAL_SERVER_ERROR.into_response()
                }
            }
        } else {
            warn!("File not found for ID: {}", uuid);
            StatusCode::NOT_FOUND.into_response()
        }
    } else {
        warn!("Invalid file ID or short code: {}", id);
        StatusCode::NOT_FOUND.into_response()
    }
}

pub fn create_app(app_state: AppState) -> Router {
    Router::new()
        .route("/health", get(health_check))
        .route("/drop", post(upload_file))
        .route("/drop/{id}", get(download_file))
        .with_state(app_state)
}

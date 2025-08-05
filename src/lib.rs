use axum::{
    Router,
    extract::{Multipart, Path, State},
    http::{StatusCode, header},
    response::{IntoResponse, Json},
    routing::{get, post},
    body::Body,
};
use color_eyre::eyre::Result;
use sanitize_filename::sanitize;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::env;
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

// In-memory storage for files (TODO: replace with database)
pub type FileStorage = Arc<Mutex<HashMap<String, FileData>>>;
// URL shortener mapping: short_code -> full_uuid
pub type ShortUrlStorage = Arc<Mutex<HashMap<String, String>>>;
// Rate limiting: IP -> (last_request_time, request_count)
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
}

impl Default for Config {
    fn default() -> Self {
        Self {
            min_file_size_limit: 50 * 1024 * 1024, // 50MB
            max_file_size_limit: 5 * 1024 * 1024 * 1024, // 5GB
            max_total_size_per_request: 10 * 1024 * 1024 * 1024, // 10GB
            stream_threshold: 50 * 1024 * 1024, // 50MB
            temp_directory: PathBuf::from("./temp"),
            bind_address: "0.0.0.0:3000".to_string(),
            memory_pool_ratio: 0.5,
            reserved_memory_mb: 200,
            rate_limit_requests_per_minute: 60,
            rate_limit_window_seconds: 60,
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
        
        config
    }
}

// Application state
#[derive(Clone)]
pub struct AppState {
    pub file_storage: FileStorage,
    pub short_url_storage: ShortUrlStorage,
    pub rate_limit_storage: RateLimitStorage,
    pub config: Config,
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

fn resolve_id_or_short_code(
    input: &str,
    short_url_storage: &ShortUrlStorage,
) -> Option<String> {
    // First check if it's a short code
    if let Ok(storage_guard) = short_url_storage.lock() {
        if let Some(full_id) = storage_guard.get(input) {
            return Some(full_id.clone());
        }
    } else {
        error!("Failed to acquire lock on short URL storage");
    }
    
    // If not found as short code, check if it looks like a UUID
    if input.len() == 36 && input.chars().nth(8) == Some('-') {
        Some(input.to_string())
    } else {
        None
    }
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
        return format!("{}...{}", &sanitized[..100], &sanitized[sanitized.len()-50..]);
    }
    
    sanitized
}

// Rate limiting check
fn check_rate_limit(
    client_ip: &str, 
    rate_storage: &RateLimitStorage, 
    config: &Config
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
    mut multipart: Multipart,
) -> Result<Json<UploadResponse>, StatusCode> {
    info!("Starting file upload");

    // Rate limiting (we'll get IP from headers in production, using placeholder for now)
    let client_ip = "127.0.0.1"; // TODO: Extract from request headers
    check_rate_limit(client_ip, &app_state.rate_limit_storage, &app_state.config)?;

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
        info!("Processing file: {} (sanitized from: {})", filename, raw_filename);

        let content_type = field
            .content_type()
            .unwrap_or("application/octet-stream") // Standard fallback for binary data
            .to_string();

        // Generate a unique ID for the file early
        let id = Uuid::new_v4().to_string();
        let short_code = generate_short_code();
        info!("Generated file ID: {}, short code: {}", id, short_code);

        // Store the short URL mapping early
        if let Ok(mut storage_guard) = app_state.short_url_storage.lock() {
            storage_guard.insert(short_code.clone(), id.clone());
        } else {
            error!("Failed to acquire lock on short URL storage during upload");
            ACTIVE_CONNECTIONS.fetch_sub(1, Ordering::Relaxed);
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }

        // Create temp directory if it doesn't exist
        ensure_temp_directory(&app_state.config.temp_directory).await?;
        
        // Always stream to disk first for large file support
        let file_path = app_state.config.temp_directory.join(format!("file_{}", id));
        let file_size = stream_field_to_disk(field, &file_path, app_state.config.max_file_size_limit).await?;

        // Check total request size limit
        total_size += file_size;
        if total_size > app_state.config.max_total_size_per_request {
            error!("Total request size exceeds maximum limit of {}", format_size(app_state.config.max_total_size_per_request));
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
        let file_data = if file_size < app_state.config.stream_threshold && try_allocate_memory(file_size) {
            info!("Moving file '{}' to memory pool (size: {})", filename, format_size(file_size));
            
            // Read file into memory and delete from disk
            match tokio::fs::read(&file_path).await {
                Ok(data) => {
                    // Delete the temporary file since we have it in memory
                    if let Err(e) = tokio::fs::remove_file(&file_path).await {
                        warn!("Failed to remove temporary file: {:?}", e);
                    }
                    
                    FileData {
                        filename: filename.clone(),
                        content_type,
                        data: Some(data),
                        file_path: None,
                    }
                }
                Err(e) => {
                    error!("Failed to read file into memory: {:?}", e);
                    deallocate_memory(file_size);
                    FileData {
                        filename: filename.clone(),
                        content_type,
                        data: None,
                        file_path: Some(file_path),
                    }
                }
            }
        } else {
            info!("Keeping file '{}' on disk (size: {})", filename, format_size(file_size));
            FileData {
                filename: filename.clone(),
                content_type,
                data: None,
                file_path: Some(file_path),
            }
        };

        if let Ok(mut storage_guard) = app_state.file_storage.lock() {
            storage_guard.insert(id.clone(), file_data);
            info!("Successfully stored file '{}' with ID: {}", filename, id);
        } else {
            error!("Failed to acquire lock on file storage during upload");
            ACTIVE_CONNECTIONS.fetch_sub(1, Ordering::Relaxed);
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }

        // Decrement active connections
        ACTIVE_CONNECTIONS.fetch_sub(1, Ordering::Relaxed);

        // Return the ID and short URL
        return Ok(Json(UploadResponse { 
            id: id.clone(),
            short_url: format!("http://{}/drop/{}", app_state.config.bind_address, short_code),
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
    let resolved_id = resolve_id_or_short_code(&id, &app_state.short_url_storage)
        .unwrap_or_else(|| id.clone());

    info!("Resolved ID: {}", resolved_id);

    let file_data = {
        match app_state.file_storage.lock() {
            Ok(storage_guard) => storage_guard.get(&resolved_id).cloned(),
            Err(e) => {
                error!("Failed to acquire lock on file storage during download: {}", e);
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
                        
                        info!(
                            "Streaming file '{}' from disk",
                            file_data.filename
                        );
                        (headers, body).into_response()
                    }
                    Err(e) => {
                        error!("Failed to open file from disk: {:?}", e);
                        StatusCode::INTERNAL_SERVER_ERROR.into_response()
                    }
                }
            }
            _ => {
                error!("Invalid file data state for ID: {}", resolved_id);
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            }
        }
    } else {
        warn!("File not found for ID: {}", resolved_id);
        StatusCode::NOT_FOUND.into_response()
    }
}

pub fn create_app(app_state: AppState) -> Router {
    Router::new()
        .route("/drop", post(upload_file))
        .route("/drop/{id}", get(download_file))
        .with_state(app_state)
}

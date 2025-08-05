use axum::{
    Router,
    extract::{Multipart, Path, State},
    http::{StatusCode, header},
    response::{IntoResponse, Json},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use sysinfo::System;
use tracing::{error, info, instrument, warn};
use tracing_subscriber;
use uuid::Uuid;

// In-memory storage for files (TODO: replace with database)
type FileStorage = Arc<Mutex<HashMap<String, FileData>>>;

// Memory pool for tracking allocated memory
static MEMORY_POOL: AtomicUsize = AtomicUsize::new(0); // Available memory pool size
static ALLOCATED_MEMORY: AtomicUsize = AtomicUsize::new(0); // Currently allocated from pool
static ACTIVE_CONNECTIONS: AtomicUsize = AtomicUsize::new(0);

const MIN_FILE_SIZE_LIMIT: usize = 50 * 1024 * 1024; // 50MB minimum per file

#[derive(Clone, Debug, Serialize, Deserialize)]
struct FileData {
    filename: String,
    content_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<Vec<u8>>, // In-memory data
    #[serde(skip_serializing_if = "Option::is_none")]
    file_path: Option<PathBuf>, // Disk-based path
}

#[derive(Serialize)]
struct UploadResponse {
    id: String,
}

fn initialize_memory_pool() {
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

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_target(false)
        .compact()
        .init();

    info!("Starting drop...💧");

    // Initialize memory pool based on system memory
    initialize_memory_pool();

    let file_storage: FileStorage = Arc::new(Mutex::new(HashMap::new()));

    let app = Router::new()
        .route("/drop", post(upload_file))
        .route("/drop/{id}", get(download_file))
        .with_state(file_storage);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    info!("Server running on http://0.0.0.0:3000");
    axum::serve(listener, app).await.unwrap();
}

#[instrument(skip(storage, multipart))]
async fn upload_file(
    State(storage): State<FileStorage>,
    mut multipart: Multipart,
) -> Result<Json<UploadResponse>, StatusCode> {
    info!("Starting file upload");

    // Increment active connections
    ACTIVE_CONNECTIONS.fetch_add(1, Ordering::Relaxed);

    // Process the multipart form data
    while let Some(field) = multipart.next_field().await.map_err(|e| {
        error!("Failed to get next field: {:?}", e);
        ACTIVE_CONNECTIONS.fetch_sub(1, Ordering::Relaxed);
        StatusCode::BAD_REQUEST
    })? {
        let filename = field.file_name().unwrap_or("unknown").to_string();
        info!("Processing file: {}", filename);

        let content_type = field
            .content_type()
            .unwrap_or("application/octet-stream") // Standard fallback for binary data
            .to_string();

        let data = field
            .bytes()
            .await
            .map_err(|e| {
                error!("Failed to read file bytes: {:?}", e);
                ACTIVE_CONNECTIONS.fetch_sub(1, Ordering::Relaxed);
                StatusCode::BAD_REQUEST
            })?
            .to_vec();

        info!(
            "File size: {} bytes, content_type: {}",
            data.len(),
            content_type
        );

        // Generate a unique ID for the file
        let id = Uuid::new_v4().to_string();
        info!("Generated file ID: {}", id);

        // Try to allocate memory from pool first, or use disk if file is too large
        let file_data = if data.len() >= MIN_FILE_SIZE_LIMIT || !try_allocate_memory(data.len()) {
            info!(
                "File '{}' exceeds memory limit or pool exhausted, storing on disk",
                filename
            );

            // Create temp directory if it doesn't exist
            tokio::fs::create_dir_all("./temp").await.map_err(|e| {
                error!("Failed to create temp directory: {:?}", e);
                ACTIVE_CONNECTIONS.fetch_sub(1, Ordering::Relaxed);
                StatusCode::INTERNAL_SERVER_ERROR
            })?;

            // Write to disk
            let file_path = PathBuf::from(format!("./temp/file_{}", id));
            tokio::fs::write(&file_path, &data).await.map_err(|e| {
                error!("Failed to write file to disk: {:?}", e);
                ACTIVE_CONNECTIONS.fetch_sub(1, Ordering::Relaxed);
                StatusCode::INTERNAL_SERVER_ERROR
            })?;

            info!("Successfully wrote file to disk: {:?}", file_path);

            FileData {
                filename: filename.clone(),
                content_type,
                data: None,
                file_path: Some(file_path),
            }
        } else {
            info!("Storing file '{}' in memory pool", filename);
            FileData {
                filename: filename.clone(),
                content_type,
                data: Some(data),
                file_path: None,
            }
        };

        storage.lock().unwrap().insert(id.clone(), file_data);
        info!("Successfully stored file '{}' with ID: {}", filename, id);

        // Decrement active connections
        ACTIVE_CONNECTIONS.fetch_sub(1, Ordering::Relaxed);

        // Return the ID
        return Ok(Json(UploadResponse { id }));
    }

    // Decrement active connections if no files found
    ACTIVE_CONNECTIONS.fetch_sub(1, Ordering::Relaxed);
    warn!("No files found in multipart request");
    Err(StatusCode::BAD_REQUEST)
}

#[instrument(skip(storage))]
async fn download_file(
    Path(id): Path<String>,
    State(storage): State<FileStorage>,
) -> impl IntoResponse {
    info!("Attempting to download file with ID: {}", id);

    let file_data = {
        let storage_guard = storage.lock().unwrap();
        storage_guard.get(&id).cloned()
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
                    "Successfully serving file '{}' from disk",
                    file_data.filename
                );

                match tokio::fs::read(path).await {
                    Ok(data) => {
                        info!(
                            "Read {} bytes from disk for file '{}'",
                            data.len(),
                            file_data.filename
                        );
                        (headers, data).into_response()
                    }
                    Err(e) => {
                        error!("Failed to read file from disk: {:?}", e);
                        StatusCode::INTERNAL_SERVER_ERROR.into_response()
                    }
                }
            }
            _ => {
                error!("Invalid file data state for ID: {}", id);
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            }
        }
    } else {
        warn!("File not found for ID: {}", id);
        StatusCode::NOT_FOUND.into_response()
    }
}

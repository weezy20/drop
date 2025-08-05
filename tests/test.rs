use reqwest::{multipart, Client};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::TcpListener;
use tempfile::TempDir;

// Import the main application's types and functions
use drop::{Config, AppState, create_app};

/// Helper function to create a test client with appropriate timeouts
fn create_test_client() -> Client {
    Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .expect("Failed to create HTTP client")
}

/// Test server wrapper that handles cleanup
pub struct TestServer {
    pub base_url: String,
    _temp_dir: TempDir, // Keep alive for cleanup
}

impl TestServer {
    /// Create a new test server with automatic cleanup
    pub async fn new() -> Self {
        // Create a unique temporary directory that will be automatically cleaned up
        let temp_dir = TempDir::new().expect("Failed to create temporary directory");
        
        // Create test configuration
        let mut config = Config::default();
        config.bind_address = "127.0.0.1:0".to_string(); // Use port 0 for automatic assignment
        config.temp_directory = temp_dir.path().to_path_buf();
        config.min_file_size_limit = 1024; // 1KB for easier testing
        config.max_file_size_limit = 10 * 1024 * 1024; // 10MB for tests
        config.stream_threshold = 1024 * 1024; // 1MB
        
        // Create shared state
        let app_state = AppState {
            config: config.clone(),
            file_storage: Arc::new(Mutex::new(HashMap::new())),
            short_url_storage: Arc::new(Mutex::new(HashMap::new())),
            rate_limit_storage: Arc::new(Mutex::new(HashMap::new())),
        };

        let app = create_app(app_state);

        let listener = TcpListener::bind("127.0.0.1:0").await.expect("Failed to bind test server");
        let addr = listener.local_addr().expect("Failed to get local address");
        let base_url = format!("http://{}", addr);
        
        // Start the server in the background
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("Test server failed to start");
        });
        
        // Give the server a moment to start
        tokio::time::sleep(Duration::from_millis(100)).await;
        
        Self {
            base_url,
            _temp_dir: temp_dir, // This will be dropped when TestServer is dropped, cleaning up the directory
        }
    }
}

/// Upload a test file and return the response JSON
async fn upload_test_file(base_url: &str, filename: &str, content: &str) -> Result<Value, Box<dyn std::error::Error>> {
    let client = create_test_client();
    
    let part = multipart::Part::text(content.to_string())
        .file_name(filename.to_string());
    
    let form = multipart::Form::new()
        .part("file", part);

    let response = client
        .post(&format!("{}/drop", base_url))
        .multipart(form)
        .send()
        .await?;

    if !response.status().is_success() {
        return Err(format!("Upload failed with status: {}", response.status()).into());
    }

    let json: Value = response.json().await?;
    Ok(json)
}

/// Download a file by ID or short code
async fn download_test_file(base_url: &str, identifier: &str) -> Result<String, Box<dyn std::error::Error>> {
    let client = create_test_client();
    
    let response = client
        .get(&format!("{}/drop/{}", base_url, identifier))
        .send()
        .await?;

    if !response.status().is_success() {
        return Err(format!("Download failed with status: {}", response.status()).into());
    }

    let content = response.text().await?;
    Ok(content)
}

#[tokio::test]
async fn test_basic_upload_download() {
    let server = TestServer::new().await;
    
    let test_content = "Hello, World! This is a test file.";
    let test_filename = "test.txt";

    // Upload file
    let upload_response = upload_test_file(&server.base_url, test_filename, test_content)
        .await
        .expect("Failed to upload test file");

    // Extract file ID and short URL
    let file_id = upload_response["id"].as_str().expect("No file ID in response");
    let short_url = upload_response["short_url"].as_str().expect("No short URL in response");
    let full_url = upload_response["full_url"].as_str().expect("No full URL in response");

    println!("Upload response: {}", serde_json::to_string_pretty(&upload_response).unwrap());
    println!("File ID: {}", file_id);
    println!("Short URL: {}", short_url);
    println!("Full URL: {}", full_url);

    // Test download by full ID
    let downloaded_content = download_test_file(&server.base_url, file_id)
        .await
        .expect("Failed to download file by ID");
    
    assert_eq!(downloaded_content, test_content, "Downloaded content doesn't match uploaded content");

    // Extract short code from short URL and test download by short code
    let short_code = short_url.split('/').last().expect("Invalid short URL format");
    let downloaded_by_short = download_test_file(&server.base_url, short_code)
        .await
        .expect("Failed to download file by short code");
    
    assert_eq!(downloaded_by_short, test_content, "Downloaded content via short code doesn't match");
} // TestServer is dropped here, automatically cleaning up temp directory

#[tokio::test]
async fn test_large_file_streaming() {
    let server = TestServer::new().await;
    
    // Create a large test file (1MB)
    let large_content = "A".repeat(1024 * 1024);
    let test_filename = "large_test.txt";

    // Upload large file
    let upload_response = upload_test_file(&server.base_url, test_filename, &large_content)
        .await
        .expect("Failed to upload large test file");

    let file_id = upload_response["id"].as_str().expect("No file ID in response");

    // Download and verify
    let downloaded_content = download_test_file(&server.base_url, file_id)
        .await
        .expect("Failed to download large file");
    
    assert_eq!(downloaded_content.len(), large_content.len(), "Large file size mismatch");
    assert_eq!(downloaded_content, large_content, "Large file content mismatch");
} // TestServer is dropped here, automatically cleaning up temp directory

#[tokio::test]
async fn test_multiple_files() {
    let server = TestServer::new().await;
    
    let files = vec![
        ("file1.txt", "Content of file 1"),
        ("file2.txt", "Content of file 2"),
        ("file3.txt", "Content of file 3"),
    ];

    let mut uploaded_files = Vec::new();

    // Upload multiple files
    for (filename, content) in &files {
        let upload_response = upload_test_file(&server.base_url, filename, content)
            .await
            .expect("Failed to upload file");
        
        let file_id = upload_response["id"].as_str().expect("No file ID in response").to_string();
        uploaded_files.push((file_id, content));
    }

    // Download and verify each file
    for (file_id, expected_content) in uploaded_files {
        let downloaded_content = download_test_file(&server.base_url, &file_id)
            .await
            .expect("Failed to download file");
        
        assert_eq!(downloaded_content, *expected_content, "File content mismatch for ID: {}", file_id);
    }
} // TestServer is dropped here, automatically cleaning up temp directory

#[tokio::test]
async fn test_file_not_found() {
    let server = TestServer::new().await;
    let client = create_test_client();
    
    // Try to download a non-existent file
    let response = client
        .get(&format!("{}/drop/nonexistent-id", server.base_url))
        .send()
        .await
        .expect("Request failed");

    assert_eq!(response.status(), 404, "Expected 404 for non-existent file");
} // TestServer is dropped here, automatically cleaning up temp directory

#[tokio::test]
async fn test_short_code_uniqueness() {
    let server = TestServer::new().await;
    let mut short_codes = std::collections::HashSet::new();
    
    // Upload multiple files and collect short codes
    for i in 0..10 {
        let content = format!("Test file content {}", i);
        let filename = format!("test{}.txt", i);
        
        let upload_response = upload_test_file(&server.base_url, &filename, &content)
            .await
            .expect("Failed to upload file");
        
        let short_url = upload_response["short_url"].as_str().expect("No short URL in response");
        let short_code = short_url.split('/').last().expect("Invalid short URL format");
        
        // Ensure short code is unique
        assert!(short_codes.insert(short_code.to_string()), "Duplicate short code: {}", short_code);
        
        // Ensure short code is 8 characters
        assert_eq!(short_code.len(), 8, "Short code should be 8 characters: {}", short_code);
        
        // Ensure short code contains only alphanumeric characters
        assert!(short_code.chars().all(|c| c.is_ascii_alphanumeric()), "Short code should be alphanumeric: {}", short_code);
    }
} // TestServer is dropped here, automatically cleaning up temp directory

#[tokio::test]
async fn test_filename_sanitization() {
    let server = TestServer::new().await;
    
    // Test various potentially problematic filenames
    let problematic_filenames = vec![
        "../../../etc/passwd",
        "..\\..\\..\\windows\\system32\\config\\sam",
        "file/with/slashes.txt",
        "file\\with\\backslashes.txt",
        "file:with:colons.txt",
        "file<with>brackets.txt",
        "file|with|pipes.txt",
        "file\"with\"quotes.txt",
        "file*with*asterisks.txt",
        "file?with?questions.txt",
    ];

    for problematic_filename in problematic_filenames {
        let content = format!("Content for {}", problematic_filename);
        
        // This should succeed - the server should sanitize the filename
        let upload_response = upload_test_file(&server.base_url, problematic_filename, &content)
            .await
            .expect("Failed to upload file with problematic filename");
        
        let file_id = upload_response["id"].as_str().expect("No file ID in response");
        
        // Should be able to download the file
        let downloaded_content = download_test_file(&server.base_url, file_id)
            .await
            .expect("Failed to download file with sanitized filename");
        
        assert_eq!(downloaded_content, content, "Content mismatch for problematic filename: {}", problematic_filename);
    }
} // TestServer is dropped here, automatically cleaning up temp directory

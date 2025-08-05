use reqwest::{multipart, Client};
use serde_json::Value;
use std::time::Duration;

const BASE_URL: &str = "http://127.0.0.1:3000";

/// Helper function to create a test client with appropriate timeouts
fn create_test_client() -> Client {
    Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .expect("Failed to create HTTP client")
}

/// Upload a test file and return the response JSON
async fn upload_test_file(filename: &str, content: &str) -> Result<Value, Box<dyn std::error::Error>> {
    let client = create_test_client();
    
    let part = multipart::Part::text(content.to_string())
        .file_name(filename.to_string());
    
    let form = multipart::Form::new()
        .part("file", part);

    let response = client
        .post(&format!("{}/drop", BASE_URL))
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
async fn download_file(identifier: &str) -> Result<String, Box<dyn std::error::Error>> {
    let client = create_test_client();
    
    let response = client
        .get(&format!("{}/drop/{}", BASE_URL, identifier))
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
    let test_content = "Hello, World! This is a test file.";
    let test_filename = "test.txt";

    // Upload file
    let upload_response = upload_test_file(test_filename, test_content)
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
    let downloaded_content = download_file(file_id)
        .await
        .expect("Failed to download file by ID");
    
    assert_eq!(downloaded_content, test_content, "Downloaded content doesn't match uploaded content");

    // Extract short code from short URL and test download by short code
    let short_code = short_url.split('/').last().expect("Invalid short URL format");
    let downloaded_by_short = download_file(short_code)
        .await
        .expect("Failed to download file by short code");
    
    assert_eq!(downloaded_by_short, test_content, "Downloaded content via short code doesn't match");
}

#[tokio::test]
async fn test_large_file_streaming() {
    // Create a large test file (1MB)
    let large_content = "A".repeat(1024 * 1024);
    let test_filename = "large_test.txt";

    // Upload large file
    let upload_response = upload_test_file(test_filename, &large_content)
        .await
        .expect("Failed to upload large test file");

    let file_id = upload_response["id"].as_str().expect("No file ID in response");

    // Download and verify
    let downloaded_content = download_file(file_id)
        .await
        .expect("Failed to download large file");
    
    assert_eq!(downloaded_content.len(), large_content.len(), "Large file size mismatch");
    assert_eq!(downloaded_content, large_content, "Large file content mismatch");
}

#[tokio::test]
async fn test_multiple_files() {
    let files = vec![
        ("file1.txt", "Content of file 1"),
        ("file2.txt", "Content of file 2"),
        ("file3.txt", "Content of file 3"),
    ];

    let mut uploaded_files = Vec::new();

    // Upload multiple files
    for (filename, content) in &files {
        let upload_response = upload_test_file(filename, content)
            .await
            .expect("Failed to upload file");
        
        let file_id = upload_response["id"].as_str().expect("No file ID in response").to_string();
        uploaded_files.push((file_id, content));
    }

    // Download and verify each file
    for (file_id, expected_content) in uploaded_files {
        let downloaded_content = download_file(&file_id)
            .await
            .expect("Failed to download file");
        
        assert_eq!(downloaded_content, *expected_content, "File content mismatch for ID: {}", file_id);
    }
}

#[tokio::test]
async fn test_file_not_found() {
    let client = create_test_client();
    
    // Try to download a non-existent file
    let response = client
        .get(&format!("{}/drop/nonexistent-id", BASE_URL))
        .send()
        .await
        .expect("Request failed");

    assert_eq!(response.status(), 404, "Expected 404 for non-existent file");
}

#[tokio::test]
async fn test_short_code_uniqueness() {
    let mut short_codes = std::collections::HashSet::new();
    
    // Upload multiple files and collect short codes
    for i in 0..10 {
        let content = format!("Test file content {}", i);
        let filename = format!("test{}.txt", i);
        
        let upload_response = upload_test_file(&filename, &content)
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
}

#[tokio::test]
async fn test_filename_sanitization() {
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
        let upload_response = upload_test_file(problematic_filename, &content)
            .await
            .expect("Failed to upload file with problematic filename");
        
        let file_id = upload_response["id"].as_str().expect("No file ID in response");
        
        // Should be able to download the file
        let downloaded_content = download_file(file_id)
            .await
            .expect("Failed to download file with sanitized filename");
        
        assert_eq!(downloaded_content, content, "Content mismatch for problematic filename: {}", problematic_filename);
    }
}

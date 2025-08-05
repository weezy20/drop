-- Create the file_mappings table to store UUID to file path mappings
CREATE TABLE IF NOT EXISTS file_mappings (
    id UUID PRIMARY KEY,
    filename VARCHAR(255) NOT NULL,
    content_type VARCHAR(100) NOT NULL,
    file_path TEXT,
    file_size BIGINT NOT NULL,
    is_in_memory BOOLEAN DEFAULT FALSE,
    created_at TIMESTAMPTZ DEFAULT NOW(),
    accessed_at TIMESTAMPTZ DEFAULT NOW(),
    access_count INTEGER DEFAULT 0,
    expires_at TIMESTAMPTZ
);

-- Create index on created_at for cleanup operations
CREATE INDEX IF NOT EXISTS idx_file_mappings_created_at ON file_mappings(created_at);

-- Create index on expires_at for cleanup operations
CREATE INDEX IF NOT EXISTS idx_file_mappings_expires_at ON file_mappings(expires_at);

-- Create index on accessed_at for LRU cleanup
CREATE INDEX IF NOT EXISTS idx_file_mappings_accessed_at ON file_mappings(accessed_at);

-- Create the short_urls table for URL shortening
CREATE TABLE IF NOT EXISTS short_urls (
    short_code VARCHAR(16) PRIMARY KEY,
    file_id UUID NOT NULL REFERENCES file_mappings(id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ DEFAULT NOW()
);

-- Create index on file_id for lookups
CREATE INDEX IF NOT EXISTS idx_short_urls_file_id ON short_urls(file_id);

-- Create the rate_limits table for rate limiting (using database for persistence)
CREATE TABLE IF NOT EXISTS rate_limits (
    client_ip TEXT PRIMARY KEY,
    request_count INTEGER DEFAULT 0,
    window_start TIMESTAMPTZ DEFAULT NOW(),
    updated_at TIMESTAMPTZ DEFAULT NOW()
);

-- Create index on updated_at for cleanup operations
CREATE INDEX IF NOT EXISTS idx_rate_limits_updated_at ON rate_limits(updated_at);

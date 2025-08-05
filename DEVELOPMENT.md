# Development Scripts

## Local Development
```bash
# Start database services only
docker-compose up -d postgres redis

# Run application locally
cargo run

# Run with hot reload (install cargo-watch first)
cargo install cargo-watch
cargo watch -x run
```

## Testing
```bash
# Unit tests
cargo test

# Integration tests with database
DATABASE_URL=postgresql://drop_user:drop_password@localhost:5432/drop cargo test

# Test health endpoint
curl http://localhost:3000/health | jq

# Test file upload
curl -X POST -F "file=@README.md" http://localhost:3000/drop | jq

# Test file download (replace with actual short code)
curl -O http://localhost:3000/drop/a1b2c3d4
```

## Production Deployment
```bash
# Build optimized binary
cargo build --release

# Run with production settings
DATABASE_URL=postgresql://drop_user:drop_password@prod-db:5432/drop \
REDIS_URL=redis://prod-redis:6379 \
DROP_BIND_ADDRESS=0.0.0.0:3000 \
DROP_MAX_FILE_SIZE_GB=10 \
DROP_RATE_LIMIT_RPM=120 \
RUST_LOG=info \
./target/release/drop
```

## Database Operations
```bash
# Connect to database
docker exec -it drop_postgres psql -U drop_user -d drop

# View tables
\dt

# Check file mappings
SELECT id, filename, file_size, is_in_memory, created_at FROM file_mappings LIMIT 10;

# Check short URLs
SELECT short_code, file_id, created_at FROM short_urls LIMIT 10;

# Cleanup old rate limits
DELETE FROM rate_limits WHERE updated_at < NOW() - INTERVAL '1 hour';
```

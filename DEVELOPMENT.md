# 🛠️ Development Guide

## 🏃 Local Development

### Prerequisites
- Rust 1.75+ (nightly for edition 2024)
- Docker and Docker Compose
- PostgreSQL client tools (optional)

### Setup
```bash
# Clone and enter directory
git clone <repo-url>
cd drop

# Copy environment configuration
cp .env.example .env

# Start database services only
docker-compose up -d postgres redis

# Run application locally with hot reload
cargo install cargo-watch
cargo watch -x run

# Or run normally
cargo run
```

## 🧪 Testing

### Unit and Integration Tests
```bash
# Run all tests
cargo test

# Run specific test
cargo test test_basic_upload_download

# Run tests with output
cargo test -- --nocapture

# Integration tests (requires Docker services running)
docker-compose up -d
cargo test --test integration_test
```

### Manual Testing
```bash
# Test health endpoint
curl http://localhost:3000/health | jq

# Test file upload
echo "Hello, World!" > test.txt
curl -X POST -F "file=@test.txt" http://localhost:3000/drop | jq

# Test file download (replace with actual short code from upload response)
curl -O http://localhost:3000/drop/a1b2c3d4

# Test large file upload
dd if=/dev/zero of=large.bin bs=1M count=100  # 100MB file
curl -X POST -F "file=@large.bin" http://localhost:3000/drop
```

## 🗄️ Database Operations

### Connect to Database
```bash
# Connect to PostgreSQL
docker exec -it drop_postgres psql -U drop_user -d drop

# Connect to Redis
docker exec -it drop_redis redis-cli
```

### Useful SQL Queries
```sql
-- View all tables
\dt

-- Check file mappings
SELECT id, filename, file_size, is_in_memory, created_at 
FROM file_mappings 
ORDER BY created_at DESC 
LIMIT 10;

-- Check short URLs
SELECT short_code, file_id, created_at 
FROM short_urls 
ORDER BY created_at DESC 
LIMIT 10;

-- Storage statistics
SELECT 
  COUNT(*) as total_files,
  SUM(file_size) as total_size_bytes,
  COUNT(*) FILTER (WHERE is_in_memory) as memory_files,
  COUNT(*) FILTER (WHERE NOT is_in_memory) as disk_files
FROM file_mappings;

-- Rate limit status
SELECT client_ip, request_count, updated_at 
FROM rate_limits 
ORDER BY updated_at DESC;

-- Cleanup old rate limits (older than 1 hour)
DELETE FROM rate_limits 
WHERE updated_at < NOW() - INTERVAL '1 hour';
```

## 🚀 Production Deployment

### Build for Production
```bash
# Build optimized binary
cargo build --release

# Check binary size and dependencies
ls -lh target/release/drop
ldd target/release/drop
```

### Production Environment
```bash
# Example production configuration
export DATABASE_URL="postgresql://drop_user:secure_password@prod-db:5432/drop"
export REDIS_URL="redis://prod-redis:6379"
export DROP_BIND_ADDRESS="0.0.0.0:3000"
export DROP_MAX_FILE_SIZE_GB="20"
export DROP_MAX_TOTAL_SIZE_GB="50"
export DROP_TEMP_DIR="/var/tmp/drop"
export DROP_RATE_LIMIT_RPM="200"
export DROP_MEMORY_POOL_RATIO="0.6"
export RUST_LOG="info"

# Run production binary
./target/release/drop
```

### Docker Production Build
```bash
# Build production Docker image
docker build -t drop:latest .

# Run with custom environment
docker run \
  --env-file .env.production \
  -p 3000:3000 \
  -v /var/tmp/drop:/tmp/drop \
  drop:latest
```

## 🔍 Debugging

### Logging
```bash
# Enable debug logging
RUST_LOG=debug cargo run

# Enable trace logging for specific module
RUST_LOG=drop::database=trace,info cargo run

# JSON structured logging
RUST_LOG=info cargo run 2>&1 | jq
```

### Performance Profiling
```bash
# Install profiling tools
cargo install flamegraph

# Generate flame graph
cargo flamegraph --bin drop

# Memory profiling with valgrind
cargo build
valgrind --tool=massif target/debug/drop
```

### Docker Debugging
```bash
# View application logs
docker-compose logs -f app

# View database logs
docker-compose logs -f postgres

# Execute commands in running container
docker exec -it drop_app sh

# Check container resource usage
docker stats drop_app drop_postgres drop_redis
```

## 📊 Monitoring in Development

### Health Monitoring
```bash
# Continuous health check
watch -n 5 'curl -s http://localhost:3000/health | jq'

# Monitor memory usage
watch -n 2 'curl -s http://localhost:3000/health | jq .storage_stats'
```

### Database Monitoring
```sql
-- Monitor active connections
SELECT count(*) FROM pg_stat_activity WHERE datname = 'drop';

-- Monitor table sizes
SELECT 
  schemaname,
  tablename,
  pg_size_pretty(pg_total_relation_size(schemaname||'.'||tablename)) as size
FROM pg_tables 
WHERE schemaname = 'public';
```

## 🛠️ Common Development Tasks

### Adding New Migrations
```bash
# Create new migration file
touch migrations/002_new_feature.sql

# Add your SQL DDL statements
echo "ALTER TABLE file_mappings ADD COLUMN new_field TEXT;" > migrations/002_new_feature.sql

# Restart application to apply migration
cargo run
```

### Code Quality
```bash
# Format code
cargo fmt

# Lint code
cargo clippy

# Check for security vulnerabilities
cargo audit

# Run all quality checks
cargo fmt && cargo clippy && cargo test && cargo audit
```

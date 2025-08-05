# Drop 💧 - High-Performance File Sharing Service

Fast file sharing with URL shortening, streaming uploads, smart memory management, and production database support.

## Features

- **Production Database**: PostgreSQL with Redis caching and automatic fallback
- **URL Shortening**: 8-character collision-resistant codes
- **Streaming**: Files >50MB stream to disk, supports up to 10GB
- **Security**: Filename sanitization, rate limiting, path traversal protection
- **Smart Memory**: Automatic memory pool with disk fallback
- **Health Monitoring**: `/health` endpoint with database status
- **High Availability**: Automatic fallback to in-memory storage when database is down
- **Configurable**: Environment variables for all settings

## Database Setup

### Quick Start with Docker Compose

```bash
# Start all services (PostgreSQL + Redis + Drop)
docker-compose up -d

# Check health
curl http://localhost:3000/health
```

### Manual Database Setup

```bash
# Copy environment config
cp .env.example .env

# Edit .env with your database credentials
DATABASE_URL=postgresql://drop_user:drop_password@localhost:5432/drop
REDIS_URL=redis://localhost:6379

# Start PostgreSQL and Redis
docker-compose up -d postgres redis

# Run the application
cargo run
```

The application will automatically:
- Connect to PostgreSQL for persistent file metadata storage
- Use Redis for fast in-memory caching (optional)
- Fall back to in-memory storage if database is unavailable
- Run database migrations on startup

## Configuration

| Variable | Default | Description |
|----------|---------|-------------|
| `DATABASE_URL` | `None` | PostgreSQL connection string |
| `REDIS_URL` | `None` | Redis connection string (optional) |
| `DROP_BIND_ADDRESS` | `0.0.0.0:3000` | Server bind address |
| `DROP_TEMP_DIR` | `./temp` | Temp file directory |
| `DROP_MAX_FILE_SIZE_GB` | `5` | Max file size (GB) |
| `DROP_MAX_TOTAL_SIZE_GB` | `10` | Max total per request (GB) |
| `DROP_STREAM_THRESHOLD_MB` | `50` | Stream threshold (MB) |
| `DROP_MEMORY_POOL_RATIO` | `0.5` | Memory pool ratio (0.0-1.0) |
| `DROP_RATE_LIMIT_RPM` | `60` | Requests per minute per IP |

## API Endpoints

### Health Check
```bash
curl http://localhost:3000/health
```

Response:
```json
{
  "status": "healthy",
  "database": "healthy",
  "memory_pool": "256 MB / 2048 MB",
  "active_connections": 0,
  "storage_stats": {
    "total_files": 42,
    "total_size": 1048576,
    "memory_files": 12,
    "memory_usage_mb": 256,
    "pool_size_mb": 2048
  }
}
```

## Quick Start

```bash
# Build and run
cargo run

# With custom config
DROP_MAX_FILE_SIZE_GB=10 DROP_BIND_ADDRESS=127.0.0.1:8080 cargo run

# Docker
docker build -t drop .
docker run -p 3000:3000 drop
```

## API Usage

### Upload
```bash
curl -X POST -F "file=@example.txt" http://localhost:3000/drop
```

Response:
```json
{
  "id": "550e8400-e29b-41d4-a716-446655440000",
  "short_url": "http://localhost:3000/drop/a1b2c3d4",
  "full_url": "http://localhost:3000/drop/550e8400-e29b-41d4-a716-446655440000"
}
```

### Download
```bash
# By short code or full ID
curl -O http://localhost:3000/drop/a1b2c3d4
curl -O http://localhost:3000/drop/550e8400-e29b-41d4-a716-446655440000
```

## Production Setup Recommendation

```bash
# Environment variables
export DROP_BIND_ADDRESS="0.0.0.0:3000"
export DROP_MAX_FILE_SIZE_GB="10"
export DROP_TEMP_DIR="/var/tmp/drop"
export DROP_RATE_LIMIT_RPM="100"

# Run
cargo run --release
```

## Testing

```bash
# Integration tests
cargo test

# Manual test
echo "test" > test.txt
curl -X POST -F "file=@test.txt" http://localhost:3000/drop
```

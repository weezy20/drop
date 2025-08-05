# Drop 💧 - High-Performance File Sharing Service

A fast, secure file sharing service with URL shortening, streaming uploads, smart memory management, and production database support.

## ✨ Features

- **🗄️ Production Ready**: PostgreSQL with Redis caching and automatic fallback
- **🔗 URL Shortening**: 8-character collision-resistant codes with base36 encoding
- **📡 Streaming Support**: Files >50MB stream to disk, supports up to 10GB uploads
- **🔒 Security First**: Filename sanitization, rate limiting, path traversal protection
- **🧠 Smart Memory**: Automatic memory pool management with disk fallback
- **💚 Health Monitoring**: Real-time `/health` endpoint with database status
- **⚡ High Availability**: Automatic fallback to in-memory storage when database is down
- **⚙️ Fully Configurable**: Environment variables for all settings

## 🚀 Quick Start

### Docker Compose (Recommended)

```bash
# Clone the repository
git clone <repo-url>
cd drop

# Start all services (PostgreSQL + Redis + Drop)
docker-compose up -d

# Check health
curl http://localhost:3000/health

# Upload a file
curl -X POST -F "file=@README.md" http://localhost:3000/drop
```

### Manual Setup

```bash
# Copy environment configuration
cp .env.example .env

# Edit .env with your settings
nano .env

# Start database services
docker-compose up -d postgres redis

# Build and run
cargo run --release
```

## 🔧 Configuration

All configuration is done via environment variables:

| Variable | Default | Description |
|----------|---------|-------------|
| `DATABASE_URL` | None | PostgreSQL connection string |
| `REDIS_URL` | None | Redis connection string (optional) |
| `DROP_BIND_ADDRESS` | `0.0.0.0:3000` | Server bind address |
| `DROP_TEMP_DIR` | `/tmp/drop` | Temporary file directory |
| `DROP_MAX_FILE_SIZE_GB` | `5` | Maximum single file size (GB) |
| `DROP_MAX_TOTAL_SIZE_GB` | `10` | Maximum total request size (GB) |
| `DROP_STREAM_THRESHOLD_MB` | `50` | Memory-to-disk threshold (MB) |
| `DROP_MEMORY_POOL_RATIO` | `0.5` | Memory pool ratio (0.0-1.0) |
| `DROP_RATE_LIMIT_RPM` | `60` | Requests per minute per IP |

## 📡 API Reference

### Health Check
```bash
GET /health
```

**Response:**
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

### Upload File
```bash
POST /drop
Content-Type: multipart/form-data
```

**Example:**
```bash
curl -X POST -F "file=@example.txt" http://localhost:3000/drop
```

**Response:**
```json
{
  "id": "550e8400-e29b-41d4-a716-446655440000",
  "short_url": "http://localhost:3000/drop/a1b2c3d4",
  "full_url": "http://localhost:3000/drop/550e8400-e29b-41d4-a716-446655440000"
}
```

### Download File
```bash
GET /drop/{id_or_short_code}
```

**Examples:**
```bash
# Download by short code
curl -O http://localhost:3000/drop/a1b2c3d4

# Download by full UUID
curl -O http://localhost:3000/drop/550e8400-e29b-41d4-a716-446655440000
```

## 🏗️ Architecture

- **Database Layer**: PostgreSQL for persistent metadata storage with automatic migrations
- **Caching Layer**: Redis for fast lookups (optional)
- **Storage Strategy**: Smart memory/disk hybrid based on file size and available memory
- **Fallback System**: Graceful degradation to in-memory storage when database is unavailable
- **Health Monitoring**: Real-time status checks for all components

## 🧪 Testing

```bash
# Run all tests
cargo test

# Run integration tests (requires Docker services)
docker-compose up -d
cargo test --test integration_test

# Manual testing
echo "Hello, World!" > test.txt
curl -X POST -F "file=@test.txt" http://localhost:3000/drop
```

## 🚀 Production Deployment

### Recommended Environment
```bash
# Production settings
export DATABASE_URL="postgresql://user:pass@localhost:5432/drop"
export REDIS_URL="redis://localhost:6379"
export DROP_BIND_ADDRESS="0.0.0.0:3000"
export DROP_MAX_FILE_SIZE_GB="10"
export DROP_TEMP_DIR="/var/tmp/drop"
export DROP_RATE_LIMIT_RPM="100"
export RUST_LOG="info"

# Build and run
cargo build --release
./target/release/drop
```

### Docker Production
```bash
# Build production image
docker build -t drop:latest .

# Run with environment file
docker run --env-file .env -p 3000:3000 drop:latest
```

## 📊 Performance Features

- **Memory Pool Management**: Automatic sizing based on system memory
- **Streaming Uploads**: Large files stream directly to disk
- **Connection Pooling**: Efficient database connections with SQLx
- **Rate Limiting**: Per-IP request limiting
- **Health Checks**: Docker and application-level health monitoring
- **Graceful Degradation**: Service continues even when database is down

## 🔒 Security Features

- **Filename Sanitization**: Prevents path traversal attacks
- **Rate Limiting**: Protection against abuse
- **Input Validation**: Comprehensive request validation
- **Error Handling**: No sensitive information leaked in errors
- **User Isolation**: Docker container runs as non-root user

## 📝 License

This project is licensed under the MIT License - see the LICENSE file for details.

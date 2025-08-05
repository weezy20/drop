# Build stage
FROM rustlang/rust:nightly-slim AS builder

# Install system dependencies
RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    libpq-dev \
    && rm -rf /var/lib/apt/lists/*

# Create app directory
WORKDIR /usr/src/app

# Copy manifests
COPY Cargo.toml Cargo.lock ./

# Copy source code and migrations
COPY src ./src
COPY migrations ./migrations

# Build the application
RUN cargo build --release

# Runtime stage
FROM debian:bookworm-slim

# Install runtime dependencies
RUN apt-get update && apt-get install -y \
    libssl3 \
    libpq5 \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Create app user
RUN useradd -r -s /bin/false -m drop

# Create temp directory
RUN mkdir -p /tmp/drop && chown drop:drop /tmp/drop

# Copy the binary from builder stage
COPY --from=builder /usr/src/app/target/release/drop /usr/local/bin/drop

# Switch to app user
USER drop

# Expose port
EXPOSE 3000

# Run the binary
CMD ["drop"]

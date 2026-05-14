# ==========================================
# Stage 1: Build Frontend
# ==========================================
FROM node:20-alpine AS frontend-builder

WORKDIR /app/frontend

# Install dependencies
COPY frontend/package.json frontend/package-lock.json* ./
RUN npm ci

# Copy frontend source and build
COPY frontend/ ./
RUN npm run build

# ==========================================
# Stage 2: Build Rust Backend
# ==========================================
FROM rust:1.75-slim-bookworm AS backend-builder

WORKDIR /app

# Install build dependencies
RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

# Create a new empty shell project for dependency caching
RUN cargo new --bin {package_name}
WORKDIR /app/{package_name}

# Copy manifests
COPY Cargo.toml Cargo.lock ./

# Build dependencies only (for caching)
RUN mkdir -p cmd && echo "fn main() {}" > cmd/main.rs
RUN cargo build --release && rm -f src/*.rs cmd/main.rs

# Copy actual source code
COPY cmd/ ./cmd/
COPY src/ ./src/

# Copy frontend build output to public directory
COPY --from=frontend-builder /app/frontend/dist ./public/assets

# Build the application (single unified binary)
RUN rm ./target/release/deps/{package_name}* 2>/dev/null || true && cargo build --release

# ==========================================
# Stage 3: Runtime Image
# ==========================================
FROM debian:bookworm-slim AS runtime

WORKDIR /app

# Install runtime dependencies
RUN apt-get update && apt-get install -y \
    ca-certificates \
    libssl3 \
    && rm -rf /var/lib/apt/lists/*

# Create non-root user
RUN useradd -m -u 1000 appuser

# Copy the compiled binary
COPY --from=backend-builder /app/{package_name}/target/release/{package_name} ./app

# Copy public assets
COPY --from=backend-builder /app/{package_name}/public ./public

# Set ownership
RUN chown -R appuser:appuser /app

USER appuser

# Environment variables
ENV APP_ENV=production
ENV SERVER_HOST=0.0.0.0
ENV SERVER_PORT=8080

EXPOSE 8080

# Default: Run web server with auto-migrations
# Override with different commands for other modes:
#   docker run myapp ./app serve --no-migrate  # Skip migrations
#   docker run myapp ./app migrate             # Run migrations only
#   docker run myapp ./app schedule:work       # Run scheduler daemon
CMD ["./app"]

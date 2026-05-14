
  # MinIO - S3-compatible Object Storage
  minio:
    image: minio/minio:latest
    container_name: {project_name}_minio
    restart: unless-stopped
    command: server /data --console-address ":9001"
    ports:
      - "${MINIO_API_PORT:-9000}:9000"     # S3 API
      - "${MINIO_CONSOLE_PORT:-9001}:9001"  # Console UI
    environment:
      MINIO_ROOT_USER: ${MINIO_ROOT_USER:-minioadmin}
      MINIO_ROOT_PASSWORD: ${MINIO_ROOT_PASSWORD:-minioadmin}
    volumes:
      - minio_data:/data
    healthcheck:
      test: ["CMD", "curl", "-f", "http://localhost:9000/minio/health/live"]
      interval: 30s
      timeout: 20s
      retries: 3

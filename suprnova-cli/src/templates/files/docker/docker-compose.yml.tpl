services:
  # PostgreSQL Database
  postgres:
    image: postgres:16-alpine
    container_name: {project_name}_postgres
    restart: unless-stopped
    environment:
      POSTGRES_USER: ${DB_USER:-suprnova}
      POSTGRES_PASSWORD: ${DB_PASSWORD:-suprnova_secret}
      POSTGRES_DB: ${DB_NAME:-suprnova_db}
    ports:
      - "${DB_PORT:-5432}:5432"
    volumes:
      - postgres_data:/var/lib/postgresql/data
    healthcheck:
      test: ["CMD-SHELL", "pg_isready -U ${DB_USER:-suprnova} -d ${DB_NAME:-suprnova_db}"]
      interval: 10s
      timeout: 5s
      retries: 5

  # Redis Cache
  redis:
    image: redis:7-alpine
    container_name: {project_name}_redis
    restart: unless-stopped
    ports:
      - "${REDIS_PORT:-6379}:6379"
    volumes:
      - redis_data:/data
    healthcheck:
      test: ["CMD", "redis-cli", "ping"]
      interval: 10s
      timeout: 5s
      retries: 5
{mailpit_service}{minio_service}
volumes:
  postgres_data:
  redis_data:{additional_volumes}

networks:
  default:
    name: {project_name}_network

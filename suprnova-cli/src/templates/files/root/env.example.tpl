APP_NAME="Suprnova Application"
APP_ENV=local
APP_DEBUG=true
APP_URL=http://localhost:8080

# 32-byte AES-256 key (URL-safe base64, no padding). Generate one with
# `suprnova key:generate` and copy it into your `.env`. Required in
# production — Suprnova fails closed on boot when APP_ENV is not
# local/dev/test and APP_KEY is unset.
APP_KEY=

SERVER_HOST=127.0.0.1
SERVER_PORT=8080

VITE_PORT=5173

# Database (SQLite by default, change to postgres://user:pass@localhost:5432/dbname for PostgreSQL)
DATABASE_URL=sqlite://./database.db
DB_MAX_CONNECTIONS=10
DB_MIN_CONNECTIONS=1
DB_CONNECT_TIMEOUT=30
DB_LOGGING=false

# Mail
MAIL_DRIVER=smtp
MAIL_HOST=localhost
MAIL_PORT=587
MAIL_USERNAME=
MAIL_PASSWORD=
MAIL_FROM_ADDRESS=hello@example.com
MAIL_FROM_NAME="Suprnova App"

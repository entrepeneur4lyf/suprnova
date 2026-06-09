APP_NAME="{project_name}"
APP_ENV=local
APP_DEBUG=true
APP_URL=http://localhost:8765

# 32-byte AES-256 key (URL-safe base64, no padding) used to encrypt
# session cookies, pagination cursors, and anything that goes through
# `suprnova::Crypt`. Generated at scaffold time by `suprnova new`;
# rotate with `suprnova key:generate`. Required in production —
# Suprnova fails closed on boot when APP_ENV is not local/dev/test
# and APP_KEY is unset.
APP_KEY={app_key}

# Backend + Vite ports. Distinctive defaults to dodge the universally
# squatted 8080/5173. `suprnova serve` treats these as a base and scans
# upward if they're busy, so two Suprnova apps can run at once without a
# clash. Set SERVER_PORT explicitly (or rely on $PORT in production) to
# pin the backend port.
SERVER_HOST=127.0.0.1
SERVER_PORT=8765

VITE_PORT=5765

# Database (SQLite by default, change to postgres://user:pass@localhost:5432/dbname for PostgreSQL)
DATABASE_URL=sqlite://./database.db
DB_MAX_CONNECTIONS=10
DB_MIN_CONNECTIONS=1
DB_CONNECT_TIMEOUT=30
DB_LOGGING=false

# Session
SESSION_LIFETIME=120
SESSION_COOKIE=suprnova_session
SESSION_SECURE=false
SESSION_PATH=/
SESSION_SAME_SITE=Lax

# Mail
MAIL_DRIVER=smtp
MAIL_HOST=localhost
MAIL_PORT=587
MAIL_USERNAME=
MAIL_PASSWORD=
MAIL_FROM_ADDRESS=hello@example.com
MAIL_FROM_NAME="Suprnova App"

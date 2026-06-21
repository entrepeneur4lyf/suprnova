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

# Distinctive default to dodge the universally squatted 8080; set
# explicitly (or rely on $PORT in production) to pin the port.
SERVER_HOST=127.0.0.1
SERVER_PORT=8765

# SQLite (default — zero config). The framework appends `?mode=rwc`
# automatically so the database file is created on first connect.
DATABASE_URL=sqlite://database.sqlite

# PostgreSQL example:
# DATABASE_URL=postgres://user:pass@localhost:5432/{package_name}

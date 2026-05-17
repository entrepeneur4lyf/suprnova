-- Notifications table — persisted notifications produced by the `database`
-- channel of the notifications subsystem.
--
-- Portable across the three first-class drivers (SQLite, Postgres, MySQL):
--   CHAR(36)        — UUID storage; identical width on all three engines.
--   VARCHAR(255)    — supported on all three; matches Laravel's default
--                     "type" / "notifiable_type" widths so consumers can
--                     migrate from a Laravel app without surprises.
--   VARCHAR(64)     — notifiable_id is stored as a string so the same
--                     schema works for i64 keys (numeric strings) and
--                     UUID keys (36 chars) without per-driver casting.
--   TEXT            — JSON-serialized data. Postgres TEXT is unbounded;
--                     SQLite uses dynamic typing; MySQL TEXT holds up to
--                     65535 bytes which exceeds any realistic notification
--                     payload.
--   TIMESTAMP       — Postgres maps this to `TIMESTAMP WITHOUT TIME ZONE`;
--                     MySQL and SQLite accept it directly. Application
--                     code is responsible for writing UTC timestamps.
CREATE TABLE notifications (
    id CHAR(36) PRIMARY KEY,                -- UUID
    type VARCHAR(255) NOT NULL,             -- notification name (e.g. "OrderShipped")
    notifiable_type VARCHAR(255) NOT NULL,  -- model class name (e.g. "users")
    notifiable_id VARCHAR(64) NOT NULL,     -- recipient id as string (portable across i64/UUID)
    data TEXT NOT NULL,                     -- JSON-serialized notification data
    read_at TIMESTAMP NULL,
    created_at TIMESTAMP NOT NULL,
    updated_at TIMESTAMP NOT NULL
);

CREATE INDEX idx_notifications_notifiable ON notifications(notifiable_type, notifiable_id);
CREATE INDEX idx_notifications_read_at ON notifications(read_at);

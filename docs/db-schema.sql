-- clipboard-manager storage schema (SQLite, WAL)
-- Applied at daemon startup; migrations tracked with PRAGMA user_version.

PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA foreign_keys = ON;
PRAGMA busy_timeout = 5000;

PRAGMA user_version = 2;

-- Metadata table for items (queried by GetHistory & SearchHistory)
CREATE TABLE IF NOT EXISTS items (
  rowid          INTEGER PRIMARY KEY,
  id             TEXT    NOT NULL UNIQUE,          -- stable external id (uuid)
  content_hash   TEXT    NOT NULL,                 -- blake3 of primary payload bytes
  primary_mime   TEXT    NOT NULL,                 -- main MIME type (e.g. text/plain, image/png)
  preview        TEXT    NOT NULL,                 -- <= 200 chars, computed at ingest
  source_app     TEXT,                             -- app id of focused window at copy time
  created_at     INTEGER NOT NULL,                 -- unix epoch milliseconds
  last_used_at   INTEGER,                          -- updated on dedup hit / reuse (milliseconds)
  pinned         INTEGER NOT NULL DEFAULT 0,
  has_thumbnail  INTEGER NOT NULL DEFAULT 0,
  size           INTEGER NOT NULL DEFAULT 0,       -- total size in bytes
  encrypted      INTEGER NOT NULL DEFAULT 0
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_items_hash    ON items (content_hash);
CREATE        INDEX IF NOT EXISTS idx_items_active  ON items (pinned DESC, COALESCE(last_used_at, created_at) DESC, rowid DESC);
CREATE        INDEX IF NOT EXISTS idx_items_created ON items (created_at DESC);
CREATE        INDEX IF NOT EXISTS idx_items_mime    ON items (primary_mime);

-- Multi-MIME payloads table (preserves rich text, plain text, and secondary formats)
CREATE TABLE IF NOT EXISTS item_payloads (
  item_id        TEXT    NOT NULL,
  mime_type      TEXT    NOT NULL,
  content_text   TEXT,                             -- text representations
  content_blob   BLOB,                             -- binary representations
  size           INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (item_id, mime_type),
  FOREIGN KEY (item_id) REFERENCES items (id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_payloads_item ON item_payloads (item_id);

-- FTS5 over the preview text. External content: text is stored in `items`.
-- Trigram tokenizer enables instant substring & subword matching across code, URLs, and paths.
CREATE VIRTUAL TABLE IF NOT EXISTS items_fts USING fts5(
  preview,
  content = 'items',
  content_rowid = 'rowid',
  tokenize = 'trigram'
);

CREATE TRIGGER IF NOT EXISTS items_fts_insert AFTER INSERT ON items BEGIN
  INSERT INTO items_fts (rowid, preview) VALUES (new.rowid, new.preview);
END;

CREATE TRIGGER IF NOT EXISTS items_fts_delete AFTER DELETE ON items BEGIN
  INSERT INTO items_fts (items_fts, rowid, preview) VALUES ('delete', old.rowid, old.preview);
END;

CREATE TRIGGER IF NOT EXISTS items_fts_update AFTER UPDATE OF preview ON items BEGIN
  INSERT INTO items_fts (items_fts, rowid, preview) VALUES ('delete', old.rowid, old.preview);
  INSERT INTO items_fts (rowid, preview) VALUES (new.rowid, new.preview);
END;

-- Settings and rules
CREATE TABLE IF NOT EXISTS settings (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS excluded_apps (
  app_id   TEXT PRIMARY KEY,
  added_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS mime_rules (
  mime_type TEXT PRIMARY KEY,
  store     INTEGER NOT NULL DEFAULT 1
);

-- Settings keys (defaults)
--   max_entries        = 2000
--   retention_days     = 30
--   max_item_bytes     = 16777216    -- 16 MB
--   thumbnail_max_px   = 200
--   encrypt_at_rest    = 0
--   filter_passwords   = 1           -- password manager MIME hints
--   filter_entropy     = 0           -- opt-in heuristic entropy filtering
--   paste_as_plain     = 0
--   auto_paste         = 1           -- automatic paste via Clutter virtual device
--   keybinding         = '<Super>V'

-- Migrations (PRAGMA user_version)
--   v1: initial schema
--   v2: timestamps are unix milliseconds. Second resolution was not enough: two copies in the
--       same second shared a sort_time, so the order between them fell back to insertion order
--       and a re-copy could not move an item to the top. Rows written before v2 hold seconds and
--       are converted; idx_items_active gains the rowid DESC tie-break.

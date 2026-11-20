use rusqlite::{Connection, Result};
use tracing::{info, warn};

pub const CURRENT_SCHEMA_VERSION: u32 = 3;

pub fn initialize_database(conn: &Connection) -> Result<()> {
    // 1. Pragmas for high concurrency, durability, and safety
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "busy_timeout", 5000)?;

    // 2. Check user_version for migrations
    let user_version: u32 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    info!("Current database schema version: {}", user_version);

    if user_version == 0 {
        apply_v1_migration(conn)?;
        conn.pragma_update(None, "user_version", 1)?;
        info!("Successfully upgraded schema to version 1");
    }

    if user_version < 2 {
        // v2: timestamps move from unix seconds to unix milliseconds, and the history
        // ordering gets a deterministic tie-break. Second resolution was not enough: two
        // copies inside the same second had the same sort_time, so the order between them
        // fell back to insertion order and a re-copy could not move an item to the top.
        apply_v2_migration(conn)?;
        conn.pragma_update(None, "user_version", 2)?;
    }

    if user_version < 3 {
        warn!("Migrating schema from version {} to {}", user_version, CURRENT_SCHEMA_VERSION);
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch("ALTER TABLE items ADD COLUMN private_metadata BLOB;
                          UPDATE items SET encrypted = 0 WHERE encrypted <> 0;
                          DELETE FROM settings WHERE key = 'encrypt_at_rest';")?;
        tx.pragma_update(None, "user_version", CURRENT_SCHEMA_VERSION)?;
        tx.commit()?;
        info!("Upgraded schema to version {}", CURRENT_SCHEMA_VERSION);
    }

    Ok(())
}

fn apply_v1_migration(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        -- Metadata table for items
        CREATE TABLE IF NOT EXISTS items (
            rowid          INTEGER PRIMARY KEY,
            id             TEXT    NOT NULL UNIQUE,
            content_hash   TEXT    NOT NULL,
            primary_mime   TEXT    NOT NULL,
            preview        TEXT    NOT NULL,
            source_app     TEXT,
            created_at     INTEGER NOT NULL,
            last_used_at   INTEGER,
            pinned         INTEGER NOT NULL DEFAULT 0,
            has_thumbnail  INTEGER NOT NULL DEFAULT 0,
            size           INTEGER NOT NULL DEFAULT 0,
            encrypted      INTEGER NOT NULL DEFAULT 0
        );

        CREATE UNIQUE INDEX IF NOT EXISTS idx_items_hash    ON items (content_hash);
        CREATE        INDEX IF NOT EXISTS idx_items_active  ON items (pinned DESC, COALESCE(last_used_at, created_at) DESC, rowid DESC);
        CREATE        INDEX IF NOT EXISTS idx_items_created ON items (created_at DESC);
        CREATE        INDEX IF NOT EXISTS idx_items_mime    ON items (primary_mime);

        -- Multi-MIME payloads table
        CREATE TABLE IF NOT EXISTS item_payloads (
            item_id        TEXT    NOT NULL,
            mime_type      TEXT    NOT NULL,
            content_text   TEXT,
            content_blob   BLOB,
            size           INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (item_id, mime_type),
            FOREIGN KEY (item_id) REFERENCES items (id) ON DELETE CASCADE
        );

        CREATE INDEX IF NOT EXISTS idx_payloads_item ON item_payloads (item_id);

        -- FTS5 trigram virtual table for subword & substring searching
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

        -- Application settings key-value store
        CREATE TABLE IF NOT EXISTS settings (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );

        -- Excluded application identifiers
        CREATE TABLE IF NOT EXISTS excluded_apps (
            app_id   TEXT PRIMARY KEY,
            added_at INTEGER NOT NULL
        );

        -- MIME storage rules
        CREATE TABLE IF NOT EXISTS mime_rules (
            mime_type TEXT PRIMARY KEY,
            store     INTEGER NOT NULL DEFAULT 1
        );
        "#
    )?;

    Ok(())
}

fn apply_v2_migration(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        -- Timestamps are unix milliseconds from v2 on. Rows written before v2 hold seconds,
        -- so they are converted rather than reinterpreted.
        UPDATE items SET created_at = created_at * 1000;
        UPDATE items SET last_used_at = last_used_at * 1000 WHERE last_used_at IS NOT NULL;
        UPDATE excluded_apps SET added_at = added_at * 1000;

        -- History ordering needs a tie-break for rows sharing a timestamp.
        DROP INDEX IF EXISTS idx_items_active;
        CREATE INDEX IF NOT EXISTS idx_items_active
            ON items (pinned DESC, COALESCE(last_used_at, created_at) DESC, rowid DESC);
        "#
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v2_migration_converts_second_timestamps() {
        let conn = Connection::open_in_memory().expect("in-memory sqlite");
        apply_v1_migration(&conn).expect("v1 migration failed");

        conn.execute(
            "INSERT INTO items (id, content_hash, primary_mime, preview, created_at, last_used_at)
             VALUES ('x', 'h', 'text/plain', 'p', 1700000000, 1700000005)",
            [],
        )
        .expect("insert failed");
        conn.execute(
            "INSERT INTO excluded_apps (app_id, added_at) VALUES ('app.a', 1700000001)",
            [],
        )
        .expect("insert failed");

        apply_v2_migration(&conn).expect("v2 migration failed");

        let (created, used): (i64, i64) = conn
            .query_row("SELECT created_at, last_used_at FROM items", [], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
            })
            .unwrap();
        assert_eq!(created, 1_700_000_000_000, "created_at must be converted to milliseconds");
        assert_eq!(used, 1_700_000_005_000, "last_used_at must be converted to milliseconds");

        let added: i64 = conn
            .query_row("SELECT added_at FROM excluded_apps", [], |r| r.get(0))
            .unwrap();
        assert_eq!(added, 1_700_000_001_000, "added_at must be converted to milliseconds");
    }

    #[test]
    fn test_schema_initialization_in_memory() {
        let conn = Connection::open_in_memory().expect("failed to open in-memory sqlite");
        initialize_database(&conn).expect("schema initialization failed");

        let version: u32 = conn.query_row("PRAGMA user_version", [], |row| row.get(0)).unwrap();
        assert_eq!(version, CURRENT_SCHEMA_VERSION);

        // Verify tables exist
        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<String>, _>>()
            .unwrap();

        assert!(tables.contains(&"items".to_string()));
        assert!(tables.contains(&"item_payloads".to_string()));
        assert!(tables.contains(&"items_fts".to_string()));
        assert!(tables.contains(&"settings".to_string()));
    }
}

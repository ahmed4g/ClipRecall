//! Settings store. Values live in the `settings` table as TEXT; the type is inferred
//! from the key so `GetSettings` can return a typed `a{sv}`.

use rusqlite::{params, Connection};
use std::collections::BTreeMap;

pub const DEFAULT_MAX_ENTRIES: i64 = 2000;
pub const DEFAULT_RETENTION_DAYS: i64 = 30;
pub const DEFAULT_MAX_ITEM_BYTES: i64 = 16 * 1024 * 1024;
pub const DEFAULT_THUMBNAIL_MAX_PX: i64 = 200;

#[derive(Debug, Clone, PartialEq)]
pub enum SettingValue {
    Bool(bool),
    Int(i64),
    Str(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub max_entries: i64,
    pub retention_days: i64,
    pub max_item_bytes: i64,
    pub thumbnail_max_px: i64,
    pub filter_passwords: bool,
    pub filter_entropy: bool,
    pub encrypt_at_rest: bool,
    pub paused: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self::defaults()
    }
}

impl Config {
    pub fn defaults() -> Self {
        Self {
            max_entries: DEFAULT_MAX_ENTRIES,
            retention_days: DEFAULT_RETENTION_DAYS,
            max_item_bytes: DEFAULT_MAX_ITEM_BYTES,
            thumbnail_max_px: DEFAULT_THUMBNAIL_MAX_PX,
            filter_passwords: true,
            filter_entropy: false,
            encrypt_at_rest: false,
            paused: false,
        }
    }

    /// Read the settings table into a Config, falling back to defaults for unknown keys.
    pub fn load(conn: &Connection) -> Self {
        let mut cfg = Self::defaults();
        let mut stmt = match conn.prepare("SELECT key, value FROM settings") {
            Ok(stmt) => stmt,
            Err(_) => return cfg,
        };
        let rows = stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)));
        if let Ok(rows) = rows {
            for (key, raw) in rows.flatten() {
                cfg.apply(&key, &raw);
            }
        }
        cfg
    }

    pub fn apply(&mut self, key: &str, raw: &str) {
        match key {
            "max_entries" => self.max_entries = parse_int(raw, DEFAULT_MAX_ENTRIES),
            "retention_days" => self.retention_days = parse_int(raw, DEFAULT_RETENTION_DAYS),
            "max_item_bytes" => self.max_item_bytes = parse_int(raw, DEFAULT_MAX_ITEM_BYTES),
            "thumbnail_max_px" => self.thumbnail_max_px = parse_int(raw, DEFAULT_THUMBNAIL_MAX_PX),
            "filter_passwords" => self.filter_passwords = parse_bool(raw),
            "filter_entropy" => self.filter_entropy = parse_bool(raw),
            "encrypt_at_rest" => self.encrypt_at_rest = parse_bool(raw),
            "paused" => self.paused = parse_bool(raw),
            _ => {}
        }
    }

    pub fn as_map(&self) -> BTreeMap<String, SettingValue> {
        let mut map = BTreeMap::new();
        map.insert("max_entries".to_string(), SettingValue::Int(self.max_entries));
        map.insert("retention_days".to_string(), SettingValue::Int(self.retention_days));
        map.insert("max_item_bytes".to_string(), SettingValue::Int(self.max_item_bytes));
        map.insert("thumbnail_max_px".to_string(), SettingValue::Int(self.thumbnail_max_px));
        map.insert("filter_passwords".to_string(), SettingValue::Bool(self.filter_passwords));
        map.insert("filter_entropy".to_string(), SettingValue::Bool(self.filter_entropy));
        map.insert("encrypt_at_rest".to_string(), SettingValue::Bool(self.encrypt_at_rest));
        map.insert("paused".to_string(), SettingValue::Bool(self.paused));
        map
    }
}

/// Parse a stored TEXT value into the type the key is documented to have.
pub fn parse_raw(key: &str, raw: &str) -> SettingValue {
    match key {
        "max_entries" | "retention_days" | "max_item_bytes" | "thumbnail_max_px" => {
            SettingValue::Int(parse_int(raw, 0))
        }
        "filter_passwords" | "filter_entropy" | "encrypt_at_rest" | "paused" => {
            SettingValue::Bool(parse_bool(raw))
        }
        _ => SettingValue::Str(raw.to_string()),
    }
}

pub fn store_setting(conn: &Connection, key: &str, raw: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO settings (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, raw],
    )?;
    Ok(())
}

fn parse_int(raw: &str, fallback: i64) -> i64 {
    // Older daemons stored GVariant display strings such as "int64 5100".
    let raw = raw.trim();
    let number = match raw.split_once(' ') {
        Some(("byte" | "int16" | "uint16" | "int32" | "uint32" | "int64" | "uint64", number)) => number,
        _ => raw,
    };
    number.parse::<i64>().unwrap_or(fallback)
}

fn parse_bool(raw: &str) -> bool {
    matches!(raw.trim(), "1" | "true" | "yes" | "on")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_documented_schema() {
        let cfg = Config::defaults();
        assert_eq!(cfg.max_entries, 2000);
        assert_eq!(cfg.retention_days, 30);
        assert_eq!(cfg.max_item_bytes, 16 * 1024 * 1024);
        assert_eq!(cfg.thumbnail_max_px, 200);
        assert!(cfg.filter_passwords);
        assert!(!cfg.filter_entropy);
    }

    #[test]
    fn settings_round_trip_through_the_table() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::initialize_database(&conn).unwrap();

        store_setting(&conn, "max_entries", "12").unwrap();
        store_setting(&conn, "retention_days", "int64 45").unwrap();
        store_setting(&conn, "auto_paste", "0").unwrap();
        store_setting(&conn, "keybinding", "<Super>c").unwrap();

        let cfg = Config::load(&conn);
        assert_eq!(cfg.max_entries, 12);
        assert_eq!(cfg.retention_days, 45);

        assert!(!cfg.as_map().contains_key("auto_paste"));
        assert!(!cfg.as_map().contains_key("keybinding"));

        // Re-setting an existing key updates rather than duplicates.
        store_setting(&conn, "max_entries", "7").unwrap();
        assert_eq!(Config::load(&conn).max_entries, 7);
    }
}

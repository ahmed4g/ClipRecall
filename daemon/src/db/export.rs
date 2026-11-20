//! Export / import (`ExportHistory`, `ImportHistory`). JSON keeps payloads so an
//! export can be re-imported byte for byte; CSV carries metadata rows only.

use crate::crypto::HistoryKey;
use crate::db::storage;
use crate::types::{DaemonError, ItemMetadata, PayloadsMap};
use base64::Engine;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write;
use std::path::Path;

const B64: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::STANDARD;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportEntry {
    pub id: String,
    pub content_hash: String,
    pub primary_mime: String,
    pub preview: String,
    pub source_app: Option<String>,
    pub created_at: u64,
    pub last_used_at: Option<u64>,
    pub pinned: bool,
    pub has_thumbnail: bool,
    pub size: usize,
    pub encrypted: bool,
    /// mime -> base64 bytes, so binary payloads survive a text round trip.
    pub payloads: HashMap<String, String>,
}

pub fn export(conn: &Connection, path: &Path, format: &str) -> Result<u32, DaemonError> {
    export_protected(conn, path, format, None)
}

pub(crate) fn export_protected(
    conn: &Connection, path: &Path, format: &str, key: Option<&HistoryKey>,
) -> Result<u32, DaemonError> {
    match format {
        "json" => export_json(conn, path, key),
        "csv" => export_csv(conn, path, key),
        other => Err(DaemonError::InvalidArgument(format!(
            "unknown export format: {other} (expected json or csv)"
        ))),
    }
}

fn export_json(conn: &Connection, path: &Path, key: Option<&HistoryKey>) -> Result<u32, DaemonError> {
    let entries = read_entries(conn, key, true)?;
    let json = serde_json::to_string_pretty(&entries).map_err(|e| DaemonError::Internal(e.to_string()))?;
    write_file(path, json.as_bytes())?;
    Ok(entries.len() as u32)
}

fn export_csv(conn: &Connection, path: &Path, key: Option<&HistoryKey>) -> Result<u32, DaemonError> {
    let entries = read_entries(conn, key, false)?;
    let mut out = String::new();
    out.push_str("id,primary_mime,preview,source_app,created_at,last_used_at,pinned,has_thumbnail,size\n");
    for e in &entries {
        let fields = [
            e.id.clone(),
            e.primary_mime.clone(),
            e.preview.clone(),
            e.source_app.clone().unwrap_or_default(),
            e.created_at.to_string(),
            e.last_used_at.map(|v| v.to_string()).unwrap_or_default(),
            (e.pinned as i64).to_string(),
            (e.has_thumbnail as i64).to_string(),
            e.size.to_string(),
        ];
        out.push_str(&fields.map(csv_field).join(","));
        out.push('\n');
    }
    write_file(path, out.as_bytes())?;
    Ok(entries.len() as u32)
}

fn csv_field(value: String) -> String {
    if value.contains(',') || value.contains('"') || value.contains('\n') || value.contains('\r') {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value
    }
}

pub fn import(conn: &Connection, path: &Path, format: &str) -> Result<u32, DaemonError> {
    import_protected(conn, path, format, None)
}

pub(crate) fn import_protected(
    conn: &Connection, path: &Path, format: &str, key: Option<&HistoryKey>,
) -> Result<u32, DaemonError> {
    let tx = conn.unchecked_transaction()?;
    let count = match format {
        "json" => import_json(&tx, path, key),
        "csv" => import_csv(&tx, path, key),
        other => Err(DaemonError::InvalidArgument(format!(
            "unknown import format: {other} (expected json or csv)"
        ))),
    }?;
    tx.commit()?;
    Ok(count)
}

fn read_entries(conn: &Connection, key: Option<&HistoryKey>, include_payloads: bool) -> Result<Vec<ExportEntry>, DaemonError> {
    let mut stmt = conn.prepare(
        "SELECT id, content_hash, primary_mime, preview, source_app, created_at, last_used_at,
                pinned, has_thumbnail, size, encrypted
         FROM items ORDER BY created_at ASC",
    )?;
    let items = stmt
        .query_map([], |row| {
            Ok(ItemMetadata {
                id: row.get(0)?,
                content_hash: row.get(1)?,
                primary_mime: row.get(2)?,
                preview: row.get(3)?,
                source_app: row.get::<_, Option<String>>(4)?,
                created_at: row.get::<_, i64>(5)? as u64,
                last_used_at: row.get::<_, Option<i64>>(6)?.map(|v| v as u64),
                pinned: row.get::<_, i64>(7)? != 0,
                has_thumbnail: row.get::<_, i64>(8)? != 0,
                size: row.get::<_, i64>(9)? as usize,
                encrypted: row.get::<_, i64>(10)? != 0,
            })
        })?
        .collect::<Result<Vec<ItemMetadata>, _>>()?;

    let mut entries = Vec::with_capacity(items.len());
    for item in items {
        let item = storage::restore_metadata(conn, item, key)?;
        let payloads = if include_payloads {
            storage::read_payloads(conn, &item, key)?.into_iter()
                .map(|(mime, bytes)| (mime, B64.encode(bytes))).collect()
        } else {
            HashMap::new()
        };
        entries.push(ExportEntry {
            id: item.id,
            content_hash: item.content_hash,
            primary_mime: item.primary_mime,
            preview: item.preview,
            source_app: item.source_app,
            created_at: item.created_at,
            last_used_at: item.last_used_at,
            pinned: item.pinned,
            has_thumbnail: item.has_thumbnail,
            size: item.size,
            encrypted: false,
            payloads,
        });
    }
    Ok(entries)
}

fn import_json(conn: &Connection, path: &Path, key: Option<&HistoryKey>) -> Result<u32, DaemonError> {
    let text = std::fs::read_to_string(path)?;
    let entries: Vec<ExportEntry> =
        serde_json::from_str(&text).map_err(|_| DaemonError::InvalidArgument("invalid history JSON".into()))?;

    let mut imported = 0;
    for entry in entries {
        let (hash, preview, private) = storage::store_metadata(&entry.id, &entry.content_hash, &entry.preview, key)?;
        let mut stmt = conn.prepare(
            "INSERT INTO items (id, content_hash, primary_mime, preview, source_app, created_at,
                                last_used_at, pinned, has_thumbnail, size, encrypted, private_metadata)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT(id) DO NOTHING",
        )?;
        let changed = stmt.execute(params![
            entry.id,
            hash,
            entry.primary_mime,
            preview,
            entry.source_app,
            entry.created_at as i64,
            entry.last_used_at.map(|v| v as i64),
            entry.pinned as i64,
            entry.has_thumbnail && key.is_none(),
            entry.size as i64,
            key.is_some(),
            private,
        ])?;
        if changed == 0 {
            continue;
        }
        imported += 1;

        let mut payloads = PayloadsMap::new();
        for (mime, encoded) in entry.payloads {
            let bytes = B64.decode(encoded)
                .map_err(|_| DaemonError::InvalidArgument("bad base64 payload".into()))?;
            payloads.insert(mime, bytes);
        }
        storage::store_payloads(conn, &entry.id, &payloads, key)?;
    }
    Ok(imported)
}

fn import_csv(conn: &Connection, path: &Path, key: Option<&HistoryKey>) -> Result<u32, DaemonError> {
    let text = std::fs::read_to_string(path)?;
    let mut lines = text.lines();
    let header = match lines.next() {
        Some(h) => h,
        None => return Ok(0),
    };
    if header != "id,primary_mime,preview,source_app,created_at,last_used_at,pinned,has_thumbnail,size" {
        return Err(DaemonError::InvalidArgument(
            "csv header must be id,primary_mime,preview,source_app,created_at,last_used_at,pinned,has_thumbnail,size".to_string(),
        ));
    }

    let mut imported = 0;
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let fields = parse_csv_line(line);
        if fields.len() != 9 {
            return Err(DaemonError::InvalidArgument(format!("csv row has {} fields", fields.len())));
        }

        let hash = blake3::hash(fields[2].as_bytes()).to_hex().to_string();
        let (hash, preview, private) = storage::store_metadata(&fields[0], &hash, &fields[2], key)?;
        let mut stmt = conn.prepare(
            "INSERT INTO items (id, content_hash, primary_mime, preview, source_app, created_at,
                                last_used_at, pinned, has_thumbnail, size, encrypted, private_metadata)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT(id) DO NOTHING",
        )?;
        let changed = stmt.execute(params![
            fields[0],
            hash,
            fields[1],
            preview,
            if fields[3].is_empty() { None } else { Some(fields[3].clone()) },
            fields[4].parse::<i64>().unwrap_or(0),
            if fields[5].is_empty() { None } else { Some(fields[5].parse::<i64>().unwrap_or(0)) },
            fields[6].parse::<i64>().unwrap_or(0),
            if key.is_none() { fields[7].parse::<i64>().unwrap_or(0) } else { 0 },
            fields[8].parse::<i64>().unwrap_or(0),
            key.is_some(),
            private,
        ])?;
        if changed > 0 {
            imported += 1;
        }
    }
    Ok(imported)
}

fn parse_csv_line(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut chars = line.chars().peekable();

    while let Some(c) = chars.next() {
        if in_quotes {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    current.push('"');
                    chars.next();
                } else {
                    in_quotes = false;
                }
            } else {
                current.push(c);
            }
        } else if c == '"' {
            in_quotes = true;
        } else if c == ',' {
            fields.push(current.clone());
            current.clear();
        } else {
            current.push(c);
        }
    }
    fields.push(current);
    fields
}

fn write_file(path: &Path, bytes: &[u8]) -> Result<(), DaemonError> {
    let parent = path.parent().filter(|dir| !dir.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    // Keep clipboard data private and publish the file only after all bytes are written.
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(bytes)?;
    file.persist(path).map_err(|err| err.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::storage;
    use crate::types::PayloadsMap;
    use tempfile::TempDir;

    #[test]
    fn json_round_trip_preserves_payloads() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::initialize_database(&conn).unwrap();

        let mut payloads = PayloadsMap::new();
        payloads.insert("text/plain".to_string(), b"hello world".to_vec());
        payloads.insert("text/html".to_string(), b"<b>hello</b>".to_vec());

        let outcome = storage::insert(&conn, &payloads, "org.example.App", &Default::default()).unwrap();
        let id = match outcome {
            storage::InsertOutcome::Stored { item, .. } => item.id,
            other => panic!("expected stored, got {other:?}"),
        };

        let dir = TempDir::new().unwrap();
        let path = dir.path().join("export.json");
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(&path, b"previous export").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(export(&conn, &path, "json").unwrap(), 1);
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);

        // Import into a fresh database and compare byte for byte.
        let conn2 = Connection::open_in_memory().unwrap();
        crate::db::schema::initialize_database(&conn2).unwrap();
        assert_eq!(import(&conn2, &path, "json").unwrap(), 1);

        let original = storage::get_payloads(&conn, &id).unwrap();
        let restored = storage::get_payloads(&conn2, &id).unwrap();
        assert_eq!(original, restored);
        assert_eq!(
            storage::get_item(&conn2, &id).unwrap().unwrap().preview,
            "hello world"
        );
    }

    #[test]
    fn csv_round_trip_and_escaping() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::initialize_database(&conn).unwrap();

        let payloads = PayloadsMap::from([("text/plain".to_string(), b"a, \"b\"\n".to_vec())]);
        storage::insert(&conn, &payloads, "org.example.App", &Default::default()).unwrap();

        let dir = TempDir::new().unwrap();
        let path = dir.path().join("export.csv");
        assert_eq!(export(&conn, &path, "csv").unwrap(), 1);

        let conn2 = Connection::open_in_memory().unwrap();
        crate::db::schema::initialize_database(&conn2).unwrap();
        assert_eq!(import(&conn2, &path, "csv").unwrap(), 1);

        let row = storage::list(&conn2, 0, 10, "all").unwrap();
        assert_eq!(row[0].2, "a, \"b\"");
    }

    #[test]
    fn unknown_format_is_rejected() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::initialize_database(&conn).unwrap();
        let dir = TempDir::new().unwrap();
        let err = export(&conn, &dir.path().join("x"), "xml").unwrap_err();
        assert!(matches!(err, DaemonError::InvalidArgument(_)));
    }
}

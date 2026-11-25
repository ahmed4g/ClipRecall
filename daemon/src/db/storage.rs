//! Phase 2 storage core: ingest, dedup, retention, pins, filters, search.
//!
//! Every function here is synchronous and runs on a blocking pool thread (see the
//! `Database::*` async wrappers), never on the D-Bus reactor.
//!
//! Privacy rule: clipboard content is never logged. Log lines carry only ids, hashes,
//! sizes, MIME types and app ids. `daemon/tests/privacy.rs` asserts this.

use crate::config::{self, Config};
use crate::crypto::HistoryKey;
use crate::types::{ContentRow, DaemonError, HistoryRow, ItemMetadata, PayloadsMap};
use nucleo_matcher::pattern::{AtomKind, CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config as MatcherConfig, Matcher, Utf32String};
use rusqlite::{params, Connection, OptionalExtension, Row};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::info;

pub const PREVIEW_MAX_CHARS: usize = 200;

/// Rows pulled from the index before fuzzy scoring. Kept small so a search over
/// 5,000 entries stays well under the 25 ms gate.
pub const SEARCH_CANDIDATES: i64 = 500;
pub const MAX_SEARCH_LIMIT: i64 = 200;

/// Password manager hint MIME types. The protocol carries no "this is a password"
/// signal, so these hints are the only reliable indicator.
pub const SENSITIVE_HINTS: &[&str] = &[
    "application/x-kde-passwordManagerHint",
    "x-kde-passwordManagerHint",
    "application/x-gnome-concealed",
    "org.nspasteboard.ConcealedType",
];

/// Opt-in heuristic: high-entropy text is usually a token, not something the user
/// means to keep in history. Off by default because it produces false positives.
///
/// Byte-level Shannon entropy alone cannot separate tokens from prose: English prose
/// measures about 4.26 bits/byte, which is *above* a naive 4.2 gate. So the rule also
/// requires the payload to look like an opaque token — no whitespace, and both letters
/// and digits present. Measured values (see `daemon/tests/storage.rs`):
///   prose 105 bytes  4.26  -> not flagged (contains spaces)
///   prose  51 bytes  4.07  -> not flagged (contains spaces)
///   hex hash 64 bytes 3.98 -> flagged
///   base64 token 50 bytes 5.56 -> flagged
pub const ENTROPY_THRESHOLD: f64 = 3.5;
pub const ENTROPY_MIN_BYTES: usize = 24;

#[derive(Debug, Clone, PartialEq)]
pub enum SkipReason {
    Empty,
    Paused,
    ExcludedApp(String),
    Sensitive,
    HighEntropy,
    MimeRule(String),
    TooLarge { bytes: usize, limit: usize },
}

#[derive(Debug, Clone, PartialEq)]
pub enum InsertOutcome {
    Stored { item: ItemMetadata, dedup_hit: bool },
    Skipped(SkipReason),
}

/// Unix epoch milliseconds. Second resolution is not enough: two copies inside the same
/// second get the same `sort_time`, so history order between them falls back to insertion
/// order and a re-copy cannot move an item to the top.
pub fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub fn is_text_mime(mime: &str) -> bool {
    mime.starts_with("text/")
        || mime == "application/json"
        || mime.ends_with("+json")
        || mime.ends_with("+xml")
}

/// Deterministic primary MIME: the format paste targets prefer, otherwise the
/// first key in sorted order so the same payload set always hashes the same way.
pub fn pick_primary_mime(payloads: &PayloadsMap) -> Option<String> {
    const PRIORITY: &[&str] = &[
        "x-special/gnome-copied-files",
        "text/uri-list",
        "text/x-uri",
        "text/plain",
        "text/plain;charset=utf-8",
        "text/html",
        "text/rtf",
        "image/png",
        "image/jpeg",
    ];
    for mime in PRIORITY {
        if payloads.contains_key(*mime) {
            return Some(mime.to_string());
        }
    }
    payloads.keys().filter(|mime| !SENSITIVE_HINTS.contains(&mime.as_str())).min().cloned()
}

pub fn compute_preview(mime: &str, bytes: &[u8]) -> String {
    if is_text_mime(mime) || mime == "x-special/gnome-copied-files" {
        let text = String::from_utf8_lossy(bytes);
        let preview = if mime == "x-special/gnome-copied-files" {
            text.strip_prefix("copy\n").or_else(|| text.strip_prefix("cut\n")).unwrap_or(&text)
        } else {
            &text
        };
        preview.split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(PREVIEW_MAX_CHARS)
            .collect()
    } else {
        format!("{mime} ({} bytes)", bytes.len())
    }
}

pub fn shannon_entropy(bytes: &[u8]) -> f64 {
    if bytes.is_empty() {
        return 0.0;
    }
    let mut counts = [0u64; 256];
    for &b in bytes {
        counts[b as usize] += 1;
    }
    let n = bytes.len() as f64;
    let mut h = 0.0;
    for c in counts {
        if c > 0 {
            let p = c as f64 / n;
            h -= p * p.log2();
        }
    }
    h
}

/// True when a text payload looks like an opaque token rather than something the user
/// wrote. Requires length, no whitespace, both letters and digits, and entropy.
pub fn looks_like_token(bytes: &[u8]) -> bool {
    if bytes.len() < ENTROPY_MIN_BYTES {
        return false;
    }
    if bytes
        .iter()
        .any(|b| matches!(b, b' ' | b'\t' | b'\n' | b'\r'))
    {
        return false;
    }
    let has_digit = bytes.iter().any(|b| b.is_ascii_digit());
    let has_alpha = bytes.iter().any(|b| b.is_ascii_alphabetic());
    has_digit && has_alpha && shannon_entropy(bytes) >= ENTROPY_THRESHOLD
}

pub fn is_sensitive(payloads: &PayloadsMap) -> bool {
    payloads.keys().any(|k| SENSITIVE_HINTS.contains(&k.as_str()))
}

pub fn is_excluded_app(conn: &Connection, app_id: &str) -> Result<bool, DaemonError> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM excluded_apps WHERE app_id = ?1",
        params![app_id],
        |row| row.get(0),
    )?;
    Ok(n > 0)
}

pub fn mime_rule_allows(conn: &Connection, mime: &str) -> Result<bool, DaemonError> {
    let store: i64 = conn
        .query_row(
            "SELECT store FROM mime_rules WHERE mime_type = ?1",
            params![mime],
            |row| row.get(0),
        )
        .unwrap_or(1);
    Ok(store != 0)
}

pub fn add_excluded_app(conn: &Connection, app_id: &str) -> Result<(), DaemonError> {
    conn.execute(
        "INSERT INTO excluded_apps (app_id, added_at) VALUES (?1, ?2) ON CONFLICT(app_id) DO NOTHING",
        params![app_id, now_millis() as i64],
    )?;
    Ok(())
}

pub fn remove_excluded_app(conn: &Connection, app_id: &str) -> Result<(), DaemonError> {
    conn.execute("DELETE FROM excluded_apps WHERE app_id = ?1", params![app_id])?;
    Ok(())
}

pub fn list_excluded_apps(conn: &Connection) -> Result<Vec<String>, DaemonError> {
    let rows = conn
        .prepare("SELECT app_id FROM excluded_apps ORDER BY app_id")?
        .query_map([], |row| row.get(0))?
        .collect::<Result<Vec<String>, _>>()?;
    Ok(rows)
}

pub fn set_mime_rule(conn: &Connection, mime: &str, store: bool) -> Result<(), DaemonError> {
    conn.execute(
        "INSERT INTO mime_rules (mime_type, store) VALUES (?1, ?2)
         ON CONFLICT(mime_type) DO UPDATE SET store = excluded.store",
        params![mime, store as i64],
    )?;
    Ok(())
}

const ITEM_COLUMNS: &str = "id, content_hash, primary_mime, preview, source_app, created_at, \
     last_used_at, pinned, has_thumbnail, size, encrypted";

const ROW_COLUMNS: &str = "id, primary_mime, preview, COALESCE(last_used_at, created_at), \
     pinned, has_thumbnail, COALESCE(source_app, '')";

fn map_item(row: &Row<'_>) -> rusqlite::Result<ItemMetadata> {
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
}

fn map_history_row(row: &Row<'_>) -> rusqlite::Result<HistoryRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get::<_, i64>(3)? as u64,
        row.get::<_, i64>(4)? != 0,
        row.get::<_, i64>(5)? != 0,
        row.get::<_, String>(6)?,
    ))
}

pub fn get_item(conn: &Connection, id: &str) -> Result<Option<ItemMetadata>, DaemonError> {
    let sql = format!("SELECT {ITEM_COLUMNS} FROM items WHERE id = ?1");
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query_map(params![id], map_item)?;
    match rows.next() {
        Some(row) => Ok(Some(row?)),
        None => Ok(None),
    }
}

fn find_by_hash(conn: &Connection, hash: &str) -> Result<Option<ItemMetadata>, DaemonError> {
    let sql = format!("SELECT {ITEM_COLUMNS} FROM items WHERE content_hash = ?1");
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query_map(params![hash], map_item)?;
    match rows.next() {
        Some(row) => Ok(Some(row?)),
        None => Ok(None),
    }
}

/// Hot path. Dedup is by blake3 of the primary payload; a hit bumps `last_used_at`
/// and moves the item to the top of history instead of storing a duplicate.
pub fn insert(
    conn: &Connection,
    payloads: &PayloadsMap,
    source_app: &str,
    config: &Config,
) -> Result<InsertOutcome, DaemonError> {
    insert_protected(conn, payloads, source_app, config, None)
}

pub(crate) fn insert_protected(
    conn: &Connection,
    payloads: &PayloadsMap,
    source_app: &str,
    config: &Config,
    key: Option<&HistoryKey>,
) -> Result<InsertOutcome, DaemonError> {
    if payloads.is_empty() {
        return Ok(InsertOutcome::Skipped(SkipReason::Empty));
    }

    if is_excluded_app(conn, source_app)? {
        return Ok(InsertOutcome::Skipped(SkipReason::ExcludedApp(source_app.to_string())));
    }
    if config.filter_passwords && is_sensitive(payloads) {
        return Ok(InsertOutcome::Skipped(SkipReason::Sensitive));
    }
    let Some(primary) = pick_primary_mime(payloads) else {
        return Ok(InsertOutcome::Skipped(SkipReason::Empty));
    };
    let primary_bytes = &payloads[&primary];
    if !mime_rule_allows(conn, &primary)? {
        return Ok(InsertOutcome::Skipped(SkipReason::MimeRule(primary.clone())));
    }

    let total: usize = payloads.iter()
        .filter(|(mime, _)| !SENSITIVE_HINTS.contains(&mime.as_str()))
        .map(|(_, bytes)| bytes.len())
        .sum();
    if total > config.max_item_bytes as usize {
        return Ok(InsertOutcome::Skipped(SkipReason::TooLarge {
            bytes: total,
            limit: config.max_item_bytes as usize,
        }));
    }
    if config.filter_entropy && is_text_mime(&primary) && looks_like_token(primary_bytes) {
        return Ok(InsertOutcome::Skipped(SkipReason::HighEntropy));
    }

    if config.encrypt_at_rest && key.is_none() {
        return Err(DaemonError::Encrypted);
    }
    let original_hash = blake3::hash(primary_bytes).to_hex().to_string();
    let hash = key.map_or_else(|| original_hash.clone(), |key| key.protected_hash(&original_hash));
    let now = now_millis();

    if let Some(mut item) = find_by_hash(conn, &hash)? {
        // A plain copy may already have the same hash as a subsequent formatted copy.
        // Keep the latest rich representation with the existing item and preserve pins.
        if item.primary_mime == primary
            && (payloads.contains_key("text/html") || payloads.contains_key("text/rtf"))
        {
            let tx = conn.unchecked_transaction()?;
            tx.execute("DELETE FROM item_payloads WHERE item_id = ?1", [&item.id])?;
            store_payloads(&tx, &item.id, payloads, key)?;
            tx.execute(
                "UPDATE items SET last_used_at = ?1, size = ?2 WHERE id = ?3",
                params![now as i64, total as i64, item.id],
            )?;
            tx.commit()?;
            item.size = total;
        } else {
            conn.execute(
                "UPDATE items SET last_used_at = ?1 WHERE id = ?2",
                params![now as i64, item.id],
            )?;
        }
        item.last_used_at = Some(now);
        let item = restore_metadata(conn, item, key)?;
        info!(
            "Dedup hit: item {} ({hash})",
            item.id
        );
        return Ok(InsertOutcome::Stored {
            item,
            dedup_hit: true,
        });
    }

    let id = uuid::Uuid::new_v4().to_string();
    let preview = compute_preview(&primary, primary_bytes);
    let (hash, stored_preview, private) = store_metadata(&id, &original_hash, &preview, key)?;

    let tx = conn.unchecked_transaction()?;
    tx.execute(
        &format!(
            "INSERT INTO items (id, content_hash, primary_mime, preview, source_app, created_at, size, encrypted, private_metadata)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)"
        ),
        params![id, hash, primary, stored_preview, source_app, now as i64, total as i64, key.is_some(), private],
    )?;
    store_payloads(&tx, &id, payloads, key)?;
    prune(&tx, config)?;
    tx.commit()?;

    info!("Stored item {} ({hash}, {total} bytes, {} payloads)", id, payloads.len());

    Ok(InsertOutcome::Stored {
        item: restore_metadata(conn, get_item(conn, &id)?.unwrap(), key)?,
        dedup_hit: false,
    })
}

pub(crate) fn store_payloads(
    conn: &Connection, id: &str, payloads: &PayloadsMap, key: Option<&HistoryKey>,
) -> Result<(), DaemonError> {
    for (mime, bytes) in payloads {
        if SENSITIVE_HINTS.contains(&mime.as_str()) {
            continue;
        }
        if let Some(key) = key {
            let encrypted = key.seal(id, &format!("payload:{mime}"), bytes)?;
            conn.execute(
                "INSERT INTO item_payloads (item_id, mime_type, content_blob, size) VALUES (?1, ?2, ?3, ?4)",
                params![id, mime, encrypted, bytes.len() as i64],
            )?;
            continue;
        }
        let text = if is_text_mime(mime) {
            std::str::from_utf8(bytes).ok()
        } else {
            None
        };
        if let Some(t) = text {
            conn.execute(
                "INSERT INTO item_payloads (item_id, mime_type, content_text, size) VALUES (?1, ?2, ?3, ?4)",
                params![id, mime, t, bytes.len() as i64],
            )?;
        } else {
            conn.execute(
                "INSERT INTO item_payloads (item_id, mime_type, content_blob, size) VALUES (?1, ?2, ?3, ?4)",
                params![id, mime, bytes, bytes.len() as i64],
            )?;
        }
    }

    Ok(())
}

/// Bumps `last_used_at` when an item is pasted or selected.
pub fn touch(conn: &Connection, id: &str) -> Result<Option<ItemMetadata>, DaemonError> {
    let now = now_millis() as i64;
    let n = conn.execute("UPDATE items SET last_used_at = ?1 WHERE id = ?2", params![now, id])?;
    if n == 0 {
        return Ok(None);
    }
    get_item(conn, id)
}

pub fn set_pinned(conn: &Connection, id: &str, pinned: bool) -> Result<bool, DaemonError> {
    let n = conn.execute(
        "UPDATE items SET pinned = ?1 WHERE id = ?2",
        params![pinned as i64, id],
    )?;
    Ok(n > 0)
}

/// Pinned items are only removed when `force` is set; otherwise this reports failure.
pub fn delete_item(conn: &Connection, id: &str, force: bool) -> Result<bool, DaemonError> {
    let pinned = conn
        .query_row("SELECT pinned FROM items WHERE id = ?1", params![id], |row| row.get::<_, i64>(0))
        .ok();

    match pinned {
        None => Ok(false),
        Some(p) if force || p == 0 => {
            conn.execute("DELETE FROM items WHERE id = ?1", params![id])?;
            Ok(true)
        }
        Some(_) => Ok(false),
    }
}

pub fn clear_history(conn: &Connection, include_pinned: bool) -> Result<u32, DaemonError> {
    let sql = if include_pinned {
        "DELETE FROM items".to_string()
    } else {
        "DELETE FROM items WHERE pinned = 0".to_string()
    };
    Ok(conn.execute(&sql, [])? as u32)
}

/// Retention: pinned items are never pruned. Non-pinned items are removed when they
/// are older than `retention_days` or when more than `max_entries` of them exist.
/// Timestamps are milliseconds, so the cutoff is computed in milliseconds too.
pub fn prune(conn: &Connection, config: &Config) -> Result<Vec<String>, DaemonError> {
    let now = now_millis() as i64;
    let cutoff = now.saturating_sub(config.retention_days as i64 * 86400 * 1000);

    let mut removed = Vec::new();

    {
        let mut stmt = conn.prepare(
            "SELECT id FROM items WHERE pinned = 0 AND COALESCE(last_used_at, created_at) < ?1",
        )?;
        let ids = stmt
            .query_map(params![cutoff], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<String>, _>>()?;
        for id in &ids {
            conn.execute("DELETE FROM items WHERE id = ?1", params![id])?;
        }
        removed.extend(ids);
    }

    {
        let mut stmt = conn.prepare(
            "SELECT id FROM items WHERE pinned = 0
             ORDER BY COALESCE(last_used_at, created_at) DESC, rowid DESC LIMIT -1 OFFSET ?1",
        )?;
        let ids = stmt
            .query_map(params![config.max_entries], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<String>, _>>()?;
        for id in &ids {
            conn.execute("DELETE FROM items WHERE id = ?1", params![id])?;
        }
        removed.extend(ids);
    }

    if removed.is_empty() {
        return Ok(removed);
    }

    info!("Pruned {} entries", removed.len());
    Ok(removed)
}

pub fn list(
    conn: &Connection,
    offset: i64,
    limit: i64,
    filter: &str,
) -> Result<Vec<HistoryRow>, DaemonError> {
    list_protected(conn, offset, limit, filter, None)
}

pub(crate) fn list_protected(
    conn: &Connection, offset: i64, limit: i64, filter: &str, key: Option<&HistoryKey>,
) -> Result<Vec<HistoryRow>, DaemonError> {
    let limit = clamp_limit(limit);
    let where_clause = filter_clause(filter)?;

    let sql = format!(
        "SELECT {ROW_COLUMNS}, private_metadata FROM items{where_clause}
         ORDER BY pinned DESC, COALESCE(last_used_at, created_at) DESC, rowid DESC LIMIT ?1 OFFSET ?2"
    );
    let mut stmt = conn.prepare(&sql)?;
    private_rows(&mut stmt, params![limit, offset.max(0)], key)
}

fn filter_clause(filter: &str) -> Result<String, DaemonError> {
    Ok(match filter {
        "" | "all" => String::new(),
        // The type filters are mutually exclusive: `text/uri-list` is a text MIME by prefix,
        // but a URI entry belongs to the `file` filter, so it must not also show up as text.
        "text" => " WHERE primary_mime LIKE 'text/%' AND primary_mime NOT IN ('text/uri-list', 'text/x-uri')".to_string(),
        "image" => " WHERE primary_mime LIKE 'image/%'".to_string(),
        "file" => " WHERE primary_mime IN ('text/uri-list', 'text/x-uri', 'x-special/gnome-copied-files')".to_string(),
        "pinned" => " WHERE pinned = 1".to_string(),
        other => return Err(DaemonError::InvalidArgument(format!("unknown filter: {other}"))),
    })
}

pub(crate) fn search_filtered(
    conn: &Connection, query: &str, limit: i64, filter: &str, key: Option<&HistoryKey>,
) -> Result<Vec<HistoryRow>, DaemonError> {
    let where_clause = filter_clause(filter)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {ROW_COLUMNS}, private_metadata FROM items{where_clause}
         ORDER BY pinned DESC, COALESCE(last_used_at, created_at) DESC, rowid DESC"
    ))?;
    let candidates = private_rows(&mut stmt, [], key)?;
    if query.trim().is_empty() {
        return Ok(candidates.into_iter().take(clamp_limit(limit) as usize).collect());
    }
    Ok(score_candidates(candidates, query.trim(), clamp_limit(limit)))
}

fn clamp_limit(limit: i64) -> i64 {
    if limit <= 0 {
        50
    } else {
        limit.min(MAX_SEARCH_LIMIT)
    }
}

struct Candidate {
    row: HistoryRow,
    score: u32,
}

/// Maccy-style instant search.
///
/// Candidates are the most recent `SEARCH_CANDIDATES` rows plus the FTS5 trigram hits
/// (or `LIKE` hits when a term is shorter than the trigram minimum). Trigram matching
/// requires a literal substring, so an out-of-order fuzzy query such as `clb` would
/// return nothing from FTS alone; scoring the recent window is what makes fuzzy work.
pub fn search(conn: &Connection, query: &str, limit: i64) -> Result<Vec<HistoryRow>, DaemonError> {
    let limit = clamp_limit(limit);
    let query = query.trim();
    if query.is_empty() {
        return list(conn, 0, limit, "all");
    }

    let words: Vec<&str> = query.split_whitespace().collect();

    let mut candidates = recent_candidates(conn)?;
    if words.iter().all(|w| w.chars().count() >= 3) {
        candidates.extend(fts_candidates(conn, &words)?);
    } else {
        candidates.extend(like_candidates(conn, &words)?);
    }

    let mut seen = std::collections::HashSet::new();
    candidates.retain(|row| seen.insert(row.0.clone()));

    Ok(score_candidates(candidates, query, limit))
}

pub(crate) fn search_protected(
    conn: &Connection, query: &str, limit: i64, key: &HistoryKey,
) -> Result<Vec<HistoryRow>, DaemonError> {
    let limit = clamp_limit(limit);
    if query.trim().is_empty() {
        return list_protected(conn, 0, limit, "all", Some(key));
    }
    let mut stmt = conn.prepare(&format!("SELECT {ROW_COLUMNS}, private_metadata FROM items
         ORDER BY pinned DESC, COALESCE(last_used_at, created_at) DESC, rowid DESC"))?;
    let candidates = private_rows(&mut stmt, [], Some(key))?;
    Ok(score_candidates(candidates, query.trim(), limit))
}

fn score_candidates(candidates: Vec<HistoryRow>, query: &str, limit: i64) -> Vec<HistoryRow> {
    let mut matcher = Matcher::new(MatcherConfig::DEFAULT);
    let pattern = Pattern::new(query, CaseMatching::Ignore, Normalization::Smart, AtomKind::Fuzzy);

    let mut scored: Vec<Candidate> = Vec::with_capacity(candidates.len());
    for row in candidates {
        let haystack = Utf32String::from(row.2.as_str());
        let score = pattern.score(haystack.slice(..), &mut matcher).unwrap_or(0);
        if score > 0 {
            scored.push(Candidate { row, score });
        }
    }

    // Score first, then pinned, then recency — Maccy-style ordering.
    scored.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then(b.row.4.cmp(&a.row.4))
            .then(b.row.3.cmp(&a.row.3))
    });

    scored.into_iter().take(limit as usize).map(|c| c.row).collect()
}

fn recent_candidates(conn: &Connection) -> Result<Vec<HistoryRow>, DaemonError> {
    let sql = format!(
        "SELECT {ROW_COLUMNS} FROM items ORDER BY pinned DESC, COALESCE(last_used_at, created_at) DESC LIMIT ?1"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params![SEARCH_CANDIDATES], map_history_row)?
        .collect::<Result<Vec<HistoryRow>, _>>()?;
    Ok(rows)
}

fn fts_candidates(conn: &Connection, words: &[&str]) -> Result<Vec<HistoryRow>, DaemonError> {
    let expr = words
        .iter()
        .map(|w| format!("\"{}\"", w.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" AND ");

    let sql = format!(
        "SELECT {ROW_COLUMNS} FROM items WHERE rowid IN
         (SELECT rowid FROM items_fts WHERE items_fts MATCH ?1) LIMIT ?2"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params![expr, SEARCH_CANDIDATES], map_history_row)?
        .collect::<Result<Vec<HistoryRow>, _>>()?;
    Ok(rows)
}

fn like_candidates(conn: &Connection, words: &[&str]) -> Result<Vec<HistoryRow>, DaemonError> {
    let clauses: Vec<String> = words
        .iter()
        .enumerate()
        .map(|(i, _)| format!("preview LIKE ?{} ESCAPE '\\'", i + 1))
        .collect();
    let sql = format!(
        "SELECT {ROW_COLUMNS} FROM items WHERE {} LIMIT ?{}",
        clauses.join(" AND "),
        words.len() + 1
    );

    let owned: Vec<String> = words.iter().map(|w| format!("%{}%", escape_like(w))).collect();
    let candidate_limit = SEARCH_CANDIDATES;
    let mut refs: Vec<&dyn rusqlite::ToSql> =
        owned.iter().map(|s| s as &dyn rusqlite::ToSql).collect();
    refs.push(&candidate_limit);

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(refs.as_slice(), map_history_row)?
        .collect::<Result<Vec<HistoryRow>, _>>()?;
    Ok(rows)
}

fn escape_like(word: &str) -> String {
    word.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

pub fn get_content(conn: &Connection, id: &str, mime: &str) -> Result<ContentRow, DaemonError> {
    get_content_protected(conn, id, mime, None)
}

pub(crate) fn get_content_protected(
    conn: &Connection, id: &str, mime: &str, key: Option<&HistoryKey>,
) -> Result<ContentRow, DaemonError> {
    let item = get_item(conn, id)?
        .ok_or_else(|| DaemonError::NotFound(format!("item {id} not found")))?;
    let mime = if mime.is_empty() { item.primary_mime.as_str() } else { mime };
    let bytes = conn.query_row(
        "SELECT content_text, content_blob FROM item_payloads WHERE item_id = ?1 AND mime_type = ?2",
        params![id, mime],
        |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, Option<Vec<u8>>>(1)?)),
    ).optional()?.ok_or_else(|| DaemonError::NotFound(format!("item {id} has no {mime} payload")))?;
    Ok((mime.to_string(), restore_payload(&item, mime, bytes.0, bytes.1, key)?))
}

pub fn get_payloads(conn: &Connection, id: &str) -> Result<PayloadsMap, DaemonError> {
    get_payloads_protected(conn, id, None)
}

pub(crate) fn get_payloads_protected(
    conn: &Connection, id: &str, key: Option<&HistoryKey>,
) -> Result<PayloadsMap, DaemonError> {
    let item = get_item(conn, id)?
        .ok_or_else(|| DaemonError::NotFound(format!("item {id} not found")))?;
    let payloads = read_payloads(conn, &item, key)?;
    if payloads.is_empty() {
        return Err(DaemonError::NotFound(format!("item {id} has no payloads")));
    }
    Ok(payloads)
}

pub(crate) fn read_payloads(
    conn: &Connection, item: &ItemMetadata, key: Option<&HistoryKey>,
) -> Result<PayloadsMap, DaemonError> {
    let mut stmt = conn.prepare(
        "SELECT mime_type, content_text, content_blob FROM item_payloads WHERE item_id = ?1",
    )?;
    let mut rows = stmt.query([&item.id])?;
    let mut payloads = PayloadsMap::new();
    while let Some(row) = rows.next()? {
        let mime: String = row.get(0)?;
        let bytes = restore_payload(item, &mime, row.get(1)?, row.get(2)?, key)?;
        payloads.insert(mime, bytes);
    }
    Ok(payloads)
}

fn restore_payload(
    item: &ItemMetadata, mime: &str, text: Option<String>, blob: Option<Vec<u8>>,
    key: Option<&HistoryKey>,
) -> Result<Vec<u8>, DaemonError> {
    if item.encrypted {
        return key.ok_or(DaemonError::Encrypted)?.open(
            &item.id, &format!("payload:{mime}"), &blob.ok_or(DaemonError::Encrypted)?,
        );
    }
    Ok(text.map(String::into_bytes).or(blob).unwrap_or_default())
}

pub(crate) fn store_metadata(
    id: &str, hash: &str, preview: &str, key: Option<&HistoryKey>,
) -> Result<(String, String, Option<Vec<u8>>), DaemonError> {
    match key {
        Some(key) => {
            let metadata = serde_json::to_vec(&(hash, preview))
                .map_err(|_| DaemonError::Internal("could not serialize history metadata".into()))?;
            Ok((key.protected_hash(hash), String::new(), Some(key.seal(id, "metadata", &metadata)?)))
        }
        None => Ok((hash.to_string(), preview.to_string(), None)),
    }
}

fn open_metadata(key: &HistoryKey, id: &str, record: &[u8]) -> Result<(String, String), DaemonError> {
    serde_json::from_slice(&key.open(id, "metadata", record)?).map_err(|_| DaemonError::Encrypted)
}

pub(crate) fn restore_metadata(
    conn: &Connection, mut item: ItemMetadata, key: Option<&HistoryKey>,
) -> Result<ItemMetadata, DaemonError> {
    if item.encrypted {
        if let Some(key) = key {
            let record: Vec<u8> = conn.query_row(
                "SELECT private_metadata FROM items WHERE id = ?1", [&item.id], |row| row.get(0),
            )?;
            (item.content_hash, item.preview) = open_metadata(key, &item.id, &record)?;
        } else {
            item.preview = "Encrypted item (keyring locked)".into();
        }
    }
    Ok(item)
}

fn private_rows(
    stmt: &mut rusqlite::Statement<'_>, args: impl rusqlite::Params, key: Option<&HistoryKey>,
) -> Result<Vec<HistoryRow>, DaemonError> {
    let mut rows = stmt.query(args)?;
    let mut result = Vec::new();
    while let Some(row) = rows.next()? {
        let mut item = map_history_row(row)?;
        if let Some(record) = row.get::<_, Option<Vec<u8>>>(7)? {
            item.2 = match key {
                Some(key) => open_metadata(key, &item.0, &record)?.1,
                None => "Encrypted item (keyring locked)".into(),
            };
        }
        result.push(item);
    }
    Ok(result)
}

pub(crate) fn convert_history(conn: &Connection, enabled: bool, key: &HistoryKey) -> Result<(), DaemonError> {
    let tx = conn.unchecked_transaction()?;
    if enabled {
        tx.execute("INSERT INTO items_fts(items_fts, rank) VALUES ('secure-delete', 1)", [])?;
    }
    let items = {
        let mut stmt = tx.prepare(&format!("SELECT {ITEM_COLUMNS} FROM items"))?;
        let items = stmt.query_map([], map_item)?.collect::<Result<Vec<_>, _>>()?;
        items
    };
    let write_key = enabled.then_some(key);
    for item in items {
        let item = restore_metadata(&tx, item, Some(key))?;
        let payloads = read_payloads(&tx, &item, Some(key))?;
        let (hash, preview, metadata) = store_metadata(&item.id, &item.content_hash, &item.preview, write_key)?;
        tx.execute("DELETE FROM item_payloads WHERE item_id = ?1", [&item.id])?;
        store_payloads(&tx, &item.id, &payloads, write_key)?;
        tx.execute(
            "UPDATE items SET content_hash = ?1, preview = ?2, private_metadata = ?3,
                encrypted = ?4, has_thumbnail = CASE WHEN ?4 THEN 0 ELSE primary_mime LIKE 'image/%' END WHERE id = ?5",
            params![hash, preview, metadata, enabled, item.id],
        )?;
    }
    config::store_setting(&tx, "encrypt_at_rest", if enabled { "1" } else { "0" })?;
    tx.commit()?;
    Ok(())
}

pub fn set_has_thumbnail(conn: &Connection, id: &str) -> Result<(), DaemonError> {
    conn.execute("UPDATE items SET has_thumbnail = 1 WHERE id = ?1", params![id])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        crate::db::schema::initialize_database(&c).unwrap();
        c
    }

    fn payloads(entries: &[(&str, &[u8])]) -> PayloadsMap {
        entries.iter().map(|(m, b)| (m.to_string(), b.to_vec())).collect()
    }

    #[test]
    fn preview_is_capped_and_collapsed() {
        let long = "x".repeat(500);
        assert_eq!(compute_preview("text/plain", long.as_bytes()).len(), PREVIEW_MAX_CHARS);
        assert_eq!(compute_preview("text/plain", "a\n b\t c".as_bytes()), "a b c");
        assert_eq!(compute_preview("image/png", &[0u8; 10]), "image/png (10 bytes)");
    }

    #[test]
    fn primary_mime_prefers_plain_text() {
        let p = payloads(&[("text/html", b"<b>x</b>"), ("text/plain", b"x")]);
        assert_eq!(pick_primary_mime(&p).unwrap(), "text/plain");
        let p = payloads(&[("application/pdf", b"x")]);
        assert_eq!(pick_primary_mime(&p).unwrap(), "application/pdf");
    }

    #[test]
    fn sensitive_hints_are_detected() {
        let p = payloads(&[("text/plain", b"hunter2"), ("application/x-kde-passwordManagerHint", b"1")]);
        assert!(is_sensitive(&p));
        assert!(!is_sensitive(&payloads(&[("text/plain", b"hello")])));
    }

    #[test]
    fn entropy_flags_tokens_but_not_prose() {
        // Tokens: opaque, no whitespace, letters + digits.
        assert!(looks_like_token(
            b"a1b2c3d4e5f6g7h8i9j0k1l2m3n4o5p6"
        ));
        assert!(looks_like_token(
            b"9f2c1ab84de70a6b3c5d8e1f2a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4c5d"
        ));
        assert!(looks_like_token(
            b"Q7dKz3pLm9xRtVbHcYsDfGnEwUaJk2oP8i6y5B4h3C1q0S7lM="
        ));

        // Prose measures 4.26 bits/byte, above the raw threshold, so the whitespace and
        // character-class conditions are what keep it out of the filter.
        let prose = b"The quick brown fox jumps over the lazy dog. The dog ran home and the fox slept on the couch by the fire.";
        assert!(shannon_entropy(prose) > ENTROPY_THRESHOLD);
        assert!(!looks_like_token(prose));
        assert!(!looks_like_token(b"https://example.com/path/to/file"));
        assert!(!looks_like_token(b"short a1b2"));
    }

    #[test]
    fn filtered_search_limits_after_type_selection() {
        let conn = conn();
        for (mime, bytes) in [
            ("text/plain", b"needle".as_slice()),
            ("text/uri-list", b"file:///needle".as_slice()),
            ("image/png", b"image needle".as_slice()),
        ] {
            insert(&conn, &payloads(&[(mime, bytes)]), "test", &Config::defaults()).unwrap();
        }
        let files = search_filtered(&conn, "needle", 1, "file", None).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].1, "text/uri-list");
        let text = search_filtered(&conn, "needle", 1, "text", None).unwrap();
        assert_eq!(text.len(), 1);
        assert_eq!(text[0].1, "text/plain");
        assert!(search_filtered(&conn, "needle", 10, "pinned", None).unwrap().is_empty());
        assert!(search_filtered(&conn, "needle", 10, "unknown", None).is_err());
    }

    #[test]
    fn unknown_filter_is_rejected() {
        let err = list(&conn(), 0, 10, "banana").unwrap_err();
        assert!(matches!(err, DaemonError::InvalidArgument(_)));
    }
}

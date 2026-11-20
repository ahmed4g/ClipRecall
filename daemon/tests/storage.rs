//! Phase 1 storage checks: the schema applies cleanly, multi-payload storage works,
//! and the FTS5 trigram index actually matches substrings and stays in sync with `items`.
//!
//! Phase 2 acceptance tests: insert, dedup hit, prune, pin, clear, trigram and fuzzy
//! search, plus the ingest filters (app exclusion, MIME rules, sensitive hints, entropy).

use clipboard_manager_daemon::config::{Config, SettingValue};
use clipboard_manager_daemon::db::storage::{self, InsertOutcome, SkipReason};
use clipboard_manager_daemon::db::Database;
use clipboard_manager_daemon::types::{DaemonError, ItemMetadata};
use rusqlite::Connection;
use std::thread::sleep;
use std::time::Duration;

fn fts_hits(conn: &rusqlite::Connection, pattern: &str) -> Vec<i64> {
    conn.prepare("SELECT rowid FROM items_fts WHERE items_fts MATCH ?1")
        .unwrap()
        .query_map([pattern], |row| row.get(0))
        .unwrap()
        .collect::<Result<Vec<i64>, _>>()
        .unwrap()
}

fn payloads(entries: &[(&str, &[u8])]) -> clipboard_manager_daemon::types::PayloadsMap {
    entries
        .iter()
        .map(|(m, b)| (m.to_string(), b.to_vec()))
        .collect()
}

/// Insert and return the stored item, failing if the ingest filter skipped it.
fn stored(conn: &Connection, p: &clipboard_manager_daemon::types::PayloadsMap, app: &str, cfg: &Config) -> ItemMetadata {
    match storage::insert(conn, p, app, cfg) {
        Ok(InsertOutcome::Stored { item, .. }) => item,
        other => panic!("expected a stored item, got {other:?}"),
    }
}

#[test]
fn paused_monitoring_survives_reopening_history() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("history.db");
    let db = Database::open_file(&path).unwrap();
    assert!(!db.is_paused());
    db.set_setting("paused", "1").unwrap();
    drop(db);

    let db = Database::open_file(&path).unwrap();
    assert_eq!(db.settings().unwrap().get("paused"), Some(&SettingValue::Bool(true)));
    let payload = payloads(&[("text/plain", b"test copy")]);
    assert_eq!(
        db.insert(&payload, "test-app").unwrap(),
        InsertOutcome::Skipped(SkipReason::Paused),
    );
    assert_eq!(db.item_count().unwrap(), 0);

    db.set_setting("paused", "0").unwrap();
    assert!(matches!(
        db.insert(&payload, "test-app").unwrap(),
        InsertOutcome::Stored { .. },
    ));
}

#[test]
fn schema_applies_cleanly_and_is_idempotent() {
    let db = Database::open_in_memory().expect("in-memory db");
    let conn = db.connection();
    let conn = conn.lock().unwrap();

    let version: u32 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, clipboard_manager_daemon::db::schema::CURRENT_SCHEMA_VERSION);

    // Applying the schema a second time must not error.
    clipboard_manager_daemon::db::schema::initialize_database(&conn).unwrap();
}

#[test]
fn trigram_index_matches_substrings_without_word_boundaries() {
    let db = Database::open_in_memory().expect("in-memory db");
    let conn = db.connection();
    let conn = conn.lock().unwrap();

    conn.execute(
        "INSERT INTO items (id, content_hash, primary_mime, preview, source_app, created_at, size)
         VALUES ('r1', 'hash1', 'text/plain', 'https://github.com/p0deje/Maccy', 'org.gnome.TextEditor', 1, 32)",
        [],
    )
    .unwrap();

    // 'hub' is inside 'github', not a standalone word.
    assert_eq!(fts_hits(&conn, "hub"), vec![1]);

    // Update trigger: the index must follow the row.
    conn.execute("UPDATE items SET preview = 'blake3 dedup' WHERE id = 'r1'", [])
        .unwrap();
    assert!(fts_hits(&conn, "hub").is_empty());
    assert_eq!(fts_hits(&conn, "ake3"), vec![1]);

    // Delete trigger.
    conn.execute("DELETE FROM items WHERE id = 'r1'", []).unwrap();
    assert!(fts_hits(&conn, "ake3").is_empty());
}

#[test]
fn multi_payload_storage_and_cascade() {
    let db = Database::open_in_memory().expect("in-memory db");
    let conn = db.connection();
    let conn = conn.lock().unwrap();

    conn.execute(
        "INSERT INTO items (id, content_hash, primary_mime, preview, created_at, size)
         VALUES ('r1', 'hash1', 'text/plain', 'rich text', 1, 20)",
        [],
    )
    .unwrap();

    conn.execute(
        "INSERT INTO item_payloads (item_id, mime_type, content_text, size) VALUES ('r1', 'text/plain', 'rich text', 9)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO item_payloads (item_id, mime_type, content_blob, size) VALUES ('r1', 'text/html', X'3c623e', 3)",
        [],
    )
    .unwrap();

    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM item_payloads WHERE item_id = 'r1'", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 2);

    conn.execute("DELETE FROM items WHERE id = 'r1'", []).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM item_payloads WHERE item_id = 'r1'", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 0, "payloads must cascade with the item");
}

// ------------------------------------------------------------------ Phase 2

#[test]
fn insert_stores_multi_mime_and_retrieves_every_payload() {
    let db = Database::open_in_memory().expect("in-memory db");
    let conn = db.connection();
    let conn = conn.lock().unwrap();
    let cfg = Config::defaults();

    let p = payloads(&[("text/html", b"<b>rich text</b>"), ("text/plain", b"rich text")]);
    let item = stored(&conn, &p, "org.gnome.TextEditor", &cfg);

    assert_eq!(item.primary_mime, "text/plain", "plain text is the paste target");
    assert_eq!(item.preview, "rich text");
    assert_eq!(item.size, 9 + 16);
    assert_eq!(item.source_app.as_deref(), Some("org.gnome.TextEditor"));
    assert_eq!(item.content_hash, blake3::hash(b"rich text").to_hex().to_string());

    let got = storage::get_payloads(&conn, &item.id).unwrap();
    assert_eq!(got.len(), 2);
    assert_eq!(got.get("text/html").unwrap(), b"<b>rich text</b>");

    // Binary payload round trip.
    let p = payloads(&[("image/png", &[0x89u8, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a])]);
    let item = stored(&conn, &p, "org.gnome.TextEditor", &cfg);
    assert_eq!(item.primary_mime, "image/png");
    assert_eq!(item.preview, "image/png (8 bytes)");
    let (mime, bytes) = storage::get_content(&conn, &item.id, "").unwrap();
    assert_eq!(mime, "image/png");
    assert_eq!(bytes, vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);
}

#[test]
fn a_rich_recopy_upgrades_the_existing_plain_item() {
    let db = Database::open_in_memory().unwrap();
    let conn = db.connection();
    let conn = conn.lock().unwrap();
    let cfg = Config::defaults();
    let plain = payloads(&[("text/plain", b"hello")]);
    let rich = payloads(&[("text/plain", b"hello"), ("text/html", b"<b>hello</b>")]);

    let initial = stored(&conn, &plain, "editor", &cfg);
    let upgraded = stored(&conn, &rich, "browser", &cfg);
    assert_eq!(upgraded.id, initial.id);
    assert_eq!(upgraded.size, 17);
    assert_eq!(storage::get_payloads(&conn, &initial.id).unwrap(), rich);

    assert_eq!(stored(&conn, &plain, "editor", &cfg).id, initial.id);
    assert_eq!(storage::get_payloads(&conn, &initial.id).unwrap(), rich);
}

#[test]
fn file_references_take_priority_over_plain_aliases() {
    let db = Database::open_in_memory().unwrap();
    let conn = db.connection();
    let conn = conn.lock().unwrap();
    let cfg = Config::defaults();

    for (mime, reference) in [
        ("x-special/gnome-copied-files", &b"cut\nfile:///absent/one"[..]),
        ("text/uri-list", &b"file:///absent/two\r\n"[..]),
        ("text/x-uri", &b"file:///absent/three"[..]),
    ] {
        let offered = payloads(&[("text/plain", b"alias"), (mime, reference)]);
        let item = stored(&conn, &offered, "file-manager", &cfg);
        assert_eq!(item.primary_mime, mime);
        assert_eq!(storage::get_content(&conn, &item.id, "").unwrap().1, reference);
    }

    assert_eq!(storage::list(&conn, 0, 10, "file").unwrap().len(), 3);
    assert!(storage::list(&conn, 0, 10, "text").unwrap().is_empty());
}

#[test]
fn dedup_hit_bumps_last_used_at_and_moves_the_item_to_the_top() {
    let db = Database::open_in_memory().expect("in-memory db");
    let conn = db.connection();
    let conn = conn.lock().unwrap();
    let cfg = Config::defaults();

    let first = stored(&conn, &payloads(&[("text/plain", b"alpha")]), "app.a", &cfg);
    // Timestamps are milliseconds; the inserts are separated so the ordering is decided by
    // the timestamps rather than by the rowid tie-break.
    sleep(Duration::from_millis(2));
    let second = stored(&conn, &payloads(&[("text/plain", b"beta")]), "app.b", &cfg);
    sleep(Duration::from_millis(2));

    // Same bytes, different app: must not create a second row.
    let again = match storage::insert(&conn, &payloads(&[("text/plain", b"alpha")]), "app.c", &cfg) {
        Ok(InsertOutcome::Stored { item, dedup_hit }) => {
            assert!(dedup_hit, "identical payload must be a dedup hit");
            item
        }
        other => panic!("expected a dedup hit, got {other:?}"),
    };

    assert_eq!(again.id, first.id);
    assert_eq!(conn.query_row("SELECT COUNT(*) FROM items", [], |r| r.get::<_, i64>(0)).unwrap(), 2);

    // History is ordered by pinned DESC, sort_time DESC, so the dedup hit comes first.
    let history = storage::list(&conn, 0, 10, "all").unwrap();
    assert_eq!(history[0].0, first.id);
    assert_eq!(history[1].0, second.id);

    // The hit must be recorded as a use.
    let item = storage::get_item(&conn, &first.id).unwrap().unwrap();
    assert!(item.last_used_at.is_some(), "dedup hit must set last_used_at");
    assert_eq!(item.sort_time(), item.last_used_at.unwrap());
}

#[test]
fn timestamps_are_milliseconds_and_a_same_second_recopy_reorders() {
    let db = Database::open_in_memory().expect("in-memory db");
    let conn = db.connection();
    let conn = conn.lock().unwrap();
    let cfg = Config::defaults();

    let now = storage::now_millis();
    assert!(
        now > 1_700_000_000_000,
        "now_millis must be milliseconds, not seconds ({now})"
    );

    // Two copies inside the same wall-clock second. With second resolution both rows had the
    // same sort_time and the order fell back to insertion order, so the re-copy below could
    // not move the older item to the top. Explicit distinct milliseconds keep this deterministic.
    let older = stored(&conn, &payloads(&[("text/plain", b"older")]), "app.a", &cfg);
    let newer = stored(&conn, &payloads(&[("text/plain", b"newer")]), "app.b", &cfg);
    conn.execute(
        "UPDATE items SET created_at = ?1, last_used_at = NULL WHERE id = ?2",
        rusqlite::params![1_700_000_000_100_i64, older.id],
    )
    .unwrap();
    conn.execute(
        "UPDATE items SET created_at = ?1, last_used_at = NULL WHERE id = ?2",
        rusqlite::params![1_700_000_000_200_i64, newer.id],
    )
    .unwrap();

    let history = storage::list(&conn, 0, 10, "all").unwrap();
    assert_eq!(history[0].0, newer.id, "the newer copy is at the top");

    // A use of the older item inside the same second must move it above the newer one.
    storage::touch(&conn, &older.id).unwrap();
    let history = storage::list(&conn, 0, 10, "all").unwrap();
    assert_eq!(history[0].0, older.id, "the use must move the older item to the top");
    assert!(history[0].3 > 1_700_000_000_200, "the use timestamp must be in milliseconds");
}

#[test]
fn pinned_items_are_never_pruned() {
    let db = Database::open_in_memory().expect("in-memory db");
    let conn = db.connection();
    let conn = conn.lock().unwrap();

    // Insert with the normal settings, then prune with aggressive ones.
    let defaults = Config::defaults();
    let pinned = stored(&conn, &payloads(&[("text/plain", b"keep me")]), "app", &defaults);
    let unpinned = stored(&conn, &payloads(&[("text/plain", b"drop me")]), "app", &defaults);

    // Backdate both so the age rule applies.
    conn.execute("UPDATE items SET created_at = 1 WHERE id IN (?1, ?2)", [pinned.id.as_str(), unpinned.id.as_str()])
        .unwrap();

    storage::set_pinned(&conn, &pinned.id, true).unwrap();

    let aggressive = Config { max_entries: 1, retention_days: 0, ..Config::defaults() };
    let removed = storage::prune(&conn, &aggressive).unwrap();

    assert!(removed.contains(&unpinned.id), "the unpinned entry must be pruned");
    assert!(!removed.contains(&pinned.id), "pinned entries are never pruned");

    let remaining: Vec<String> = conn
        .prepare("SELECT id FROM items ORDER BY id")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<Vec<String>, _>>()
        .unwrap();
    assert_eq!(remaining, vec![pinned.id.clone()]);

    // Deleting a pinned item is refused unless forced.
    assert!(!storage::delete_item(&conn, &pinned.id, false).unwrap());
    assert!(storage::delete_item(&conn, &pinned.id, true).unwrap());
    assert!(!storage::delete_item(&conn, &pinned.id, true).unwrap(), "already gone");
}

#[test]
fn max_entries_prune_keeps_the_most_recent_and_pinned() {
    let db = Database::open_in_memory().expect("in-memory db");
    let conn = db.connection();
    let conn = conn.lock().unwrap();

    let defaults = Config::defaults();
    let mut ids = Vec::new();
    for i in 0..5 {
        let item = stored(&conn, &payloads(&[(
            "text/plain",
            format!("entry number {i}").as_bytes(),
        )]), "app", &defaults);
        ids.push(item.id);
    }

    // Give every entry a distinct but recent age so the recency window is
    // deterministic without tripping the age rule.
    let now = storage::now_millis() as i64;
    for (i, id) in ids.iter().enumerate() {
        let age = now - (5 - i as i64) * 1000;
        conn.execute("UPDATE items SET created_at = ?1 WHERE id = ?2", rusqlite::params![age, id])
            .unwrap();
    }

    // The pinned entry is the oldest, so it sits outside the max_entries window, but
    // pinned entries must survive pruning.
    storage::set_pinned(&conn, &ids[0], true).unwrap();
    let cfg = Config { max_entries: 3, retention_days: 3650, ..Config::defaults() };
    let removed = storage::prune(&conn, &cfg).unwrap();

    assert_eq!(conn.query_row("SELECT COUNT(*) FROM items", [], |r| r.get::<_, i64>(0)).unwrap(), 4);
    assert_eq!(removed.len(), 1);
    assert_eq!(removed[0], ids[1], "the oldest unpinned entry is the one dropped");

    let kept: Vec<String> = conn
        .prepare("SELECT id FROM items ORDER BY rowid")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<Vec<String>, _>>()
        .unwrap();
    assert_eq!(kept, vec![ids[0].clone(), ids[2].clone(), ids[3].clone(), ids[4].clone()]);
}

#[test]
fn clear_history_respects_pinned() {
    let db = Database::open_in_memory().expect("in-memory db");
    let conn = db.connection();
    let conn = conn.lock().unwrap();
    let cfg = Config::defaults();

    let a = stored(&conn, &payloads(&[("text/plain", b"one")]), "app", &cfg);
    stored(&conn, &payloads(&[("text/plain", b"two")]), "app", &cfg);
    storage::set_pinned(&conn, &a.id, true).unwrap();

    assert_eq!(storage::clear_history(&conn, false).unwrap(), 1);
    assert_eq!(conn.query_row("SELECT COUNT(*) FROM items", [], |r| r.get::<_, i64>(0)).unwrap(), 1);

    assert_eq!(storage::clear_history(&conn, true).unwrap(), 1);
    assert_eq!(conn.query_row("SELECT COUNT(*) FROM items", [], |r| r.get::<_, i64>(0)).unwrap(), 0);

    // Payloads go with them.
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM item_payloads", [], |r| r.get::<_, i64>(0)).unwrap(),
        0
    );
}

#[test]
fn trigram_search_matches_substrings() {
    let db = Database::open_in_memory().expect("in-memory db");
    let conn = db.connection();
    let conn = conn.lock().unwrap();
    let cfg = Config::defaults();

    let a = stored(&conn, &payloads(&[("text/plain", b"https://github.com/p0deje/Maccy")]), "app", &cfg);
    stored(&conn, &payloads(&[("text/plain", b"unrelated note")]), "app", &cfg);

    // 'hub' is inside 'github'; 'mac' is inside 'Maccy'.
    let hits = storage::search(&conn, "hub", 10).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].0, a.id);

    let hits = storage::search(&conn, "mac maccy", 10).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].0, a.id);

    // Case insensitive.
    assert_eq!(storage::search(&conn, "MACCY", 10).unwrap()[0].0, a.id);
}

#[test]
fn fuzzy_search_matches_out_of_order_characters() {
    let db = Database::open_in_memory().expect("in-memory db");
    let conn = db.connection();
    let conn = conn.lock().unwrap();
    let cfg = Config::defaults();

    let a = stored(&conn, &payloads(&[("text/plain", b"clipboard manager popup")]), "app", &cfg);
    stored(&conn, &payloads(&[("text/plain", b"nothing similar here")]), "app", &cfg);

    // 'clb' is not a contiguous trigram in 'clipboard', so FTS alone would miss it.
    let hits = storage::search(&conn, "clb", 10).unwrap();
    assert_eq!(hits.len(), 1, "fuzzy scoring must find the out-of-order match");
    assert_eq!(hits[0].0, a.id);

    // A pinned match outranks a slightly better-scoring unpinned one.
    let b = stored(&conn, &payloads(&[("text/plain", b"clapboard")]), "app", &cfg);
    storage::set_pinned(&conn, &b.id, true).unwrap();
    let hits = storage::search(&conn, "clb", 10).unwrap();
    assert_eq!(hits[0].0, b.id, "pinned outranks within the same query");
}

#[test]
fn history_filters_select_by_type_and_pinned() {
    let db = Database::open_in_memory().expect("in-memory db");
    let conn = db.connection();
    let conn = conn.lock().unwrap();
    let cfg = Config::defaults();

    let text = stored(&conn, &payloads(&[("text/plain", b"a note")]), "app", &cfg);
    let image = stored(&conn, &payloads(&[("image/png", b"\x89PNG")]), "app", &cfg);
    let file = stored(&conn, &payloads(&[("text/uri-list", b"file:///tmp/a")]), "app", &cfg);

    storage::set_pinned(&conn, &image.id, true).unwrap();

    let ids = |filter: &str| storage::list(&conn, 0, 10, filter)
        .unwrap()
        .into_iter()
        .map(|row| row.0)
        .collect::<Vec<String>>();

    assert_eq!(ids("all").len(), 3);
    assert_eq!(ids("text"), vec![text.id.clone()]);
    assert_eq!(ids("image"), vec![image.id.clone()]);
    assert_eq!(ids("file"), vec![file.id.clone()]);
    assert_eq!(ids("pinned"), vec![image.id.clone()]);

    // An empty filter is the same as "all", which is what GetHistory sends by default.
    assert_eq!(ids("").len(), 3);

    // The filter is matched against a fixed set, not interpolated into the SQL.
    assert!(matches!(
        storage::list(&conn, 0, 10, "text' OR 1=1 --"),
        Err(DaemonError::InvalidArgument(_))
    ));
}

#[test]
fn ingest_filters_skip_excluded_apps_mime_rules_and_sensitive_content() {
    let db = Database::open_in_memory().expect("in-memory db");
    let conn = db.connection();
    let conn = conn.lock().unwrap();
    let cfg = Config::defaults();

    storage::add_excluded_app(&conn, "org.example.Vault").unwrap();
    let skipped = storage::insert(&conn, &payloads(&[("text/plain", b"secret")]), "org.example.Vault", &cfg).unwrap();
    assert_eq!(skipped, InsertOutcome::Skipped(SkipReason::ExcludedApp("org.example.Vault".to_string())));
    storage::remove_excluded_app(&conn, "org.example.Vault").unwrap();

    // Password manager hint is the only reliable signal the protocol carries.
    let p = payloads(&[("text/plain", b"hunter2"), ("application/x-kde-passwordManagerHint", b"1")]);
    assert_eq!(
        storage::insert(&conn, &p, "app", &cfg).unwrap(),
        InsertOutcome::Skipped(SkipReason::Sensitive)
    );

    let filter_disabled = Config { filter_passwords: false, ..Config::defaults() };
    let item = stored(&conn, &p, "app", &filter_disabled);
    assert_eq!(storage::get_payloads(&conn, &item.id).unwrap(), payloads(&[("text/plain", b"hunter2")]));
    let marker_only = payloads(&[("application/x-gnome-concealed", b"")]);
    assert_eq!(
        storage::insert(&conn, &marker_only, "app", &filter_disabled).unwrap(),
        InsertOutcome::Skipped(SkipReason::Empty)
    );

    // MIME rule.
    storage::set_mime_rule(&conn, "image/svg+xml", false).unwrap();
    assert_eq!(
        storage::insert(&conn, &payloads(&[("image/svg+xml", b"<svg/>")]), "app", &cfg).unwrap(),
        InsertOutcome::Skipped(SkipReason::MimeRule("image/svg+xml".to_string()))
    );

    // Size limit.
    let tight = Config { max_item_bytes: 4, ..Config::defaults() };
    assert_eq!(
        storage::insert(&conn, &payloads(&[("text/plain", b"too long")]), "app", &tight).unwrap(),
        InsertOutcome::Skipped(SkipReason::TooLarge { bytes: 8, limit: 4 })
    );

    // Entropy filter is off by default and opt-in.
    let token = b"9f2c1ab84de70a6b3c5d8e1f2a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4c5d";
    assert!(matches!(
        storage::insert(&conn, &payloads(&[("text/plain", token)]), "app", &cfg).unwrap(),
        InsertOutcome::Stored { .. }
    ));
    let opt_in = Config { filter_entropy: true, ..Config::defaults() };
    assert_eq!(
        storage::insert(&conn, &payloads(&[("text/plain", token)]), "app", &opt_in).unwrap(),
        InsertOutcome::Skipped(SkipReason::HighEntropy)
    );

    // Empty payloads are never stored.
    assert_eq!(
        storage::insert(&conn, &payloads(&[]), "app", &cfg).unwrap(),
        InsertOutcome::Skipped(SkipReason::Empty)
    );
}

#[test]
fn excluded_app_list_round_trips() {
    let db = Database::open_in_memory().expect("in-memory db");
    let conn = db.connection();
    let conn = conn.lock().unwrap();

    storage::add_excluded_app(&conn, "org.example.Vault").unwrap();
    storage::add_excluded_app(&conn, "org.example.Ide").unwrap();
    storage::add_excluded_app(&conn, "org.example.Vault").unwrap(); // ON CONFLICT DO NOTHING

    assert_eq!(
        storage::list_excluded_apps(&conn).unwrap(),
        vec!["org.example.Ide".to_string(), "org.example.Vault".to_string()]
    );

    storage::remove_excluded_app(&conn, "org.example.Vault").unwrap();
    assert_eq!(storage::list_excluded_apps(&conn).unwrap(), vec!["org.example.Ide".to_string()]);
}

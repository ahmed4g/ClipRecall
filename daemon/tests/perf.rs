//! Phase 2 performance gates from `docs/testing.md`:
//!   - 5,000 synthetic entries
//!   - search completes in < 25 ms
//!   - insert completes in < 5 ms
//!   - database stays under 100 MB
//!
//! Measured in release mode (`cargo test --release --test perf`); the debug profile is
//! several times slower and is not what the gate describes, so a debug run asserts a
//! relaxed budget instead of failing on noise.

use clipboard_manager_daemon::config::Config;
use clipboard_manager_daemon::db::storage;
use rusqlite::Connection;
use std::time::Instant;

const ENTRIES: usize = 5000;

/// Deterministic pseudo-random text so the gate is reproducible run to run.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 32
    }
}

fn synthetic_text(rng: &mut Lcg, words: &[&str]) -> String {
    let n = 6 + (rng.next() % 12) as usize;
    (0..n)
        .map(|_| words[(rng.next() as usize) % words.len()])
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
fn five_thousand_entries_meet_the_insert_search_and_size_gates() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("perf.db");
    let conn = Connection::open(&path).unwrap();
    clipboard_manager_daemon::db::schema::initialize_database(&conn).unwrap();

    // Keep all 5,000 entries so the search gate measures the full set.
    let cfg = Config {
        max_entries: ENTRIES as i64,
        retention_days: 3650,
        ..Config::defaults()
    };
    let words = [
        "clipboard", "manager", "search", "preview", "thumbnail", "pinned", "history", "daemon",
        "extension", "wayland", "mutter", "selection", "payload", "dedup", "retention",
    ];
    let apps = ["org.gnome.TextEditor", "org.mozilla.Firefox", "com.example.Ide"];

    let mut rng = Lcg(0x9e3779b97f4a7c15);
    let mut insert_times = Vec::with_capacity(ENTRIES);

    for i in 0..ENTRIES {
        let text = synthetic_text(&mut rng, &words);
        let mut p = clipboard_manager_daemon::types::PayloadsMap::new();
        p.insert("text/plain".to_string(), text.as_bytes().to_vec());

        let started = Instant::now();
        match storage::insert(&conn, &p, apps[i % apps.len()], &cfg) {
            Ok(storage::InsertOutcome::Stored { .. }) => {}
            other => panic!("synthetic entry {i} was not stored: {other:?}"),
        }
        insert_times.push(started.elapsed().as_micros());
    }

    let total_insert: u128 = insert_times.iter().sum();
    let mean_insert = total_insert / ENTRIES as u128;
    let mut sorted = insert_times.clone();
    sorted.sort_unstable();
    let p95 = sorted[(ENTRIES as u128 * 95 / 100) as usize];

    println!("insert: mean {mean_insert} us, p95 {p95} us over {ENTRIES} entries");
    // The documented gates are release-mode measurements; the debug profile is several
    // times slower, so debug runs assert a relaxed budget instead of failing spuriously.
    let (insert_gate, search_gate) = if cfg!(debug_assertions) {
        (25_000_u128, 100_000_u128)
    } else {
        (5_000_u128, 25_000_u128)
    };
    assert!(
        mean_insert < insert_gate,
        "mean insert {mean_insert} us exceeds the {} gate",
        if cfg!(debug_assertions) { "debug budget" } else { "5 ms gate" }
    );

    // Search gate.
    let queries = ["clipboard", "hub", "thumb", "pin", "way", "dedup", "search preview", "clb", "mn"];
    let mut worst = 0_u128;
    for q in queries {
        let started = Instant::now();
        let hits = storage::search(&conn, q, 50).unwrap();
        let elapsed = started.elapsed().as_micros();
        worst = worst.max(elapsed);
        assert!(!hits.is_empty(), "query {q:?} returned nothing");
    }
    println!("search: worst {worst} us over {} queries", queries.len());
    assert!(
        worst < search_gate,
        "worst search {worst} us exceeds the {} gate",
        if cfg!(debug_assertions) { "debug budget" } else { "25 ms" }
    );

    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM items", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, ENTRIES as i64, "all synthetic entries must still be present");

    let db_bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    println!("database: {db_bytes} bytes");
    assert!(db_bytes < 100 * 1024 * 1024, "database is {db_bytes} bytes");
}

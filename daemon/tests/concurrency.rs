//! Phase 2 acceptance: no storage call runs on the D-Bus reactor thread.
//!
//! The storage core is synchronous rusqlite and `Database::off_reactor` runs it on
//! tokio's blocking pool. These tests put a long storage call in flight and check that
//! an unrelated call is answered while it is still running, which is only possible if
//! the reactor itself is never blocked.

mod common;

use clipboard_manager_daemon::db::Database;
use clipboard_manager_daemon::types::{HealthRow, HistoryRow, PayloadsMap};
use std::time::{Duration, Instant};

/// A call that touches no storage must be answered within this budget.
const NON_STORAGE_BUDGET: Duration = Duration::from_millis(10);

/// `Health` reads the entry count off the reactor, so it may have to wait for the read
/// connection, but it must not be made to wait for a call it is not blocking.
const HEALTH_BUDGET: Duration = Duration::from_millis(100);

const ENTRIES: usize = 5000;

fn seed(db: &Database, count: usize) {
    // Keep every seeded entry; the default retention window would prune them.
    db.set_setting("max_entries", &count.to_string()).expect("set max_entries");
    for i in 0..count {
        let mut p = PayloadsMap::new();
        p.insert(
            "text/plain".to_string(),
            format!("seed entry {i} alpha beta gamma").into_bytes(),
        );
        db.insert(&p, "concurrency-seed").expect("seed insert");
    }
}

async fn call<B, R>(proxy: &zbus::Proxy<'_>, method: &str, args: &B) -> R
where
    B: serde::ser::Serialize + zbus::zvariant::DynamicType,
    R: for<'d> zbus::zvariant::DynamicDeserialize<'d>,
{
    common::timeout_call(proxy, method, args, common::CALL_TIMEOUT).await
}

async fn timeout_call<B, R>(
    proxy: &zbus::Proxy<'_>,
    method: &str,
    args: &B,
    budget: Duration,
) -> R
where
    B: serde::ser::Serialize + zbus::zvariant::DynamicType,
    R: for<'d> zbus::zvariant::DynamicDeserialize<'d>,
{
    common::timeout_call(proxy, method, args, budget).await
}

#[tokio::test]
async fn a_non_storage_call_is_answered_while_a_large_ingest_is_in_flight() {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let db = Database::open_file(dir.path().join("concurrency.db")).expect("file db");
    seed(&db, ENTRIES);

    let mut harness = match common::Harness::start(db).await {
        Some(harness) => harness,
        None => return,
    };
    let proxy = harness.proxy().await;

    // Big enough that hashing and the blob write take measurably long.
    let big = vec![7u8; 12 * 1024 * 1024];
    let mut payloads = PayloadsMap::new();
    payloads.insert("application/octet-stream".to_string(), big);

    // The storage call is dispatched first, so the probe lands while it is running.
    let ingest = async {
        let started = Instant::now();
        let id: String = call(&proxy, "SubmitItem", &(&payloads, "concurrency-test")).await;
        (id, started.elapsed())
    };

    let probe = async {
        let started = Instant::now();
        common::timeout_call_void(&proxy, "IgnoreNext", &(), NON_STORAGE_BUDGET).await;
        let ignore_at = started.elapsed();

        let started = Instant::now();
        let health: HealthRow = timeout_call(&proxy, "Health", &(), HEALTH_BUDGET).await;
        (ignore_at, started.elapsed(), health)
    };

    let (ingest, probe) = tokio::join!(ingest, probe);
    let (id, ingest_at) = ingest;
    let (ignore_at, health_at, health) = probe;

    assert!(!id.is_empty(), "the ingest was skipped");
    assert!(
        health.3,
        "Health reported the daemon idle while the ingest was in flight"
    );
    assert!(
        health_at < ingest_at,
        "Health was only answered after the ingest finished ({health_at:?} vs {ingest_at:?}), \
         which means the reactor was blocked by storage work"
    );
    assert!(
        ignore_at < ingest_at,
        "IgnoreNext was only answered after the ingest finished ({ignore_at:?} vs {ingest_at:?}), \
         which means the reactor was blocked by storage work"
    );

    harness.stop();
}

#[tokio::test]
async fn a_large_search_does_not_block_the_reactor() {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let db = Database::open_file(dir.path().join("search.db")).expect("file db");
    seed(&db, ENTRIES);

    let mut harness = match common::Harness::start(db).await {
        Some(harness) => harness,
        None => return,
    };
    let proxy = harness.proxy().await;

    let search = async {
        let started = Instant::now();
        let rows: Vec<HistoryRow> = call(&proxy, "SearchHistory", &("alpha", 50i32)).await;
        (rows, started.elapsed())
    };

    let probe = async {
        let started = Instant::now();
        common::timeout_call_void(&proxy, "IgnoreNext", &(), NON_STORAGE_BUDGET).await;
        started.elapsed()
    };

    let (search, probe) = tokio::join!(search, probe);
    let (rows, search_at) = search;
    let probe_at = probe;

    assert_eq!(rows.len(), 50, "the search did not use the seeded history");
    assert!(
        probe_at < search_at,
        "IgnoreNext was only answered after the search finished ({probe_at:?} vs {search_at:?}), \
         which means the reactor was blocked by storage work"
    );

    harness.stop();
}

#[tokio::test]
async fn a_read_is_not_blocked_by_the_write_connection() {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let db = Database::open_file(dir.path().join("wal.db")).expect("file db");
    seed(&db, 2000);

    // Hold the write connection so no ingest can complete, then read.
    let write = db.connection();
    let write_guard = write.lock().expect("write lock");

    let mut harness = match common::Harness::start(db).await {
        Some(harness) => harness,
        None => return,
    };
    let proxy = harness.proxy().await;

    let rows: Vec<HistoryRow> =
        timeout_call(&proxy, "SearchHistory", &("alpha", 10i32), Duration::from_secs(3)).await;
    assert!(
        !rows.is_empty(),
        "the read connection was blocked behind the write connection"
    );

    drop(write_guard);
    harness.stop();
}

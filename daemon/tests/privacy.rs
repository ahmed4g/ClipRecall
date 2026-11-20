//! Privacy gate from `docs/testing.md`: no raw clipboard string ever appears in the
//! logs. The daemon logs ids, hashes, sizes and MIME types only.
//!
//! The test installs a capturing `tracing` subscriber, runs the code paths that would
//! be most likely to leak (ingest, preview, search, content read, thumbnail), and
//! asserts the secret never shows up in any emitted line.

use clipboard_manager_daemon::config::Config;
use clipboard_manager_daemon::db::storage;
use clipboard_manager_daemon::types::PayloadsMap;
use rusqlite::Connection;
use std::sync::{Arc, Mutex};
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

const SECRET: &str = "ZQXV-SECRET-CONTENT-9f2c1ab84de70a6b";

#[derive(Clone)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl<'writer> MakeWriter<'writer> for Capture {
    type Writer = Sink;

    fn make_writer(&self) -> Self::Writer {
        Sink(self.0.clone())
    }
}

struct Sink(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Sink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn clipboard_content_never_appears_in_the_logs() {
    let captured = Arc::new(Mutex::new(Vec::new()));

    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new("info"))
        .with(tracing_subscriber::fmt::layer().with_writer(Capture(captured.clone())))
        .init();

    let conn = Connection::open_in_memory().unwrap();
    clipboard_manager_daemon::db::schema::initialize_database(&conn).unwrap();
    // The entropy filter is off by default; the secret is stored, so every path below
    // touches the payload.
    let cfg = Config::defaults();

    let mut p = PayloadsMap::new();
    p.insert("text/plain".to_string(), SECRET.as_bytes().to_vec());
    p.insert("text/html".to_string(), format!("<b>{SECRET}</b>").into_bytes());

    let outcome = storage::insert(&conn, &p, SECRET, &cfg).unwrap();
    let id = match outcome {
        storage::InsertOutcome::Stored { item, .. } => item.id,
        other => panic!("expected the secret to be stored, got {other:?}"),
    };

    // The skipped path is the one most likely to log the rejected text.
    let opt_in = Config { filter_entropy: true, ..Config::defaults() };
    storage::insert(&conn, &p, "org.example.App", &opt_in).unwrap();
    storage::add_excluded_app(&conn, "org.example.Vault").unwrap();
    storage::insert(&conn, &p, "org.example.Vault", &cfg).unwrap();

    // Every path that touches the payload.
    storage::search(&conn, "secret", 10).unwrap();
    storage::get_content(&conn, &id, "text/plain").unwrap();
    storage::get_payloads(&conn, &id).unwrap();
    storage::set_pinned(&conn, &id, true).unwrap();
    storage::prune(&conn, &cfg).unwrap();
    let dir = tempfile::TempDir::new().unwrap();
    let export_path = dir.path().join("export.json");
    clipboard_manager_daemon::db::export::export(&conn, &export_path, "json").unwrap();

    let logs = String::from_utf8_lossy(&captured.lock().unwrap().clone()).to_string();
    println!("captured logs:\n{logs}");

    assert!(!logs.is_empty(), "the capturing subscriber saw nothing at all");
    assert!(!logs.contains(SECRET), "clipboard content leaked into the logs: {logs}");

    // The hash is fine to log, but it must not be the content itself.
    assert!(logs.contains(&blake3::hash(SECRET.as_bytes()).to_hex().to_string()));
}

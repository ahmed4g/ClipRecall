pub mod export;
pub mod schema;
pub mod storage;

use crate::config::{self, Config};
use crate::crypto::HistoryKey;
use crate::types::{ContentRow, DaemonError, HistoryRow, ItemMetadata, PayloadsMap};
use rusqlite::{Connection, OptionalExtension, Result as SqlResult};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use tracing::info;

/// Storage handle.
///
/// The synchronous methods are the storage core; the `*_async` wrappers run them on
/// tokio's blocking pool so a rusqlite call never occupies the D-Bus reactor thread.
/// `daemon/tests/concurrency.rs` asserts that distinction.
///
/// File-backed databases get two connections: one for writes and one for reads. WAL
/// mode allows a reader to work while the writer is busy, so a large ingest does not
/// stall a search. An in-memory database has a single connection, so both handles share
/// it.
#[derive(Clone)]
pub struct Database {
    write: Arc<Mutex<Connection>>,
    read: Arc<Mutex<Connection>>,
    db_path: Option<PathBuf>,
    config: Arc<RwLock<Config>>,
    busy: Arc<AtomicUsize>,
    history_key: Arc<RwLock<Option<Arc<HistoryKey>>>>,
}

struct BusyGuard(Arc<AtomicUsize>);

impl Drop for BusyGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

impl Database {
    pub fn open_file<P: AsRef<Path>>(path: P) -> SqlResult<Self> {
        let p = path.as_ref();
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        info!("Opening database at {}", p.display());

        let write = Connection::open(p)?;
        schema::initialize_database(&write)?;

        // Second connection for reads; WAL lets it run while the writer is busy.
        let read = Connection::open(p)?;
        schema::initialize_database(&read)?;

        let config = config::Config::load(&write);
        Ok(Self {
            write: Arc::new(Mutex::new(write)),
            read: Arc::new(Mutex::new(read)),
            db_path: Some(p.to_path_buf()),
            config: Arc::new(RwLock::new(config)),
            busy: Arc::new(AtomicUsize::new(0)),
            history_key: Arc::new(RwLock::new(None)),
        })
    }

    pub fn open_in_memory() -> SqlResult<Self> {
        let conn = Connection::open_in_memory()?;
        schema::initialize_database(&conn)?;
        let config = config::Config::load(&conn);
        let shared = Arc::new(Mutex::new(conn));
        Ok(Self {
            read: shared.clone(),
            write: shared,
            db_path: None,
            config: Arc::new(RwLock::new(config)),
            busy: Arc::new(AtomicUsize::new(0)),
            history_key: Arc::new(RwLock::new(None)),
        })
    }

    /// Shared handle used by tests that need to hold the write connection.
    pub fn connection(&self) -> Arc<Mutex<Connection>> {
        self.write.clone()
    }

    pub fn config(&self) -> Config {
        self.config.read().unwrap().clone()
    }

    pub fn is_paused(&self) -> bool {
        self.config.read().unwrap().paused
    }

    /// True while a storage operation is executing on the blocking pool.
    pub fn is_busy(&self) -> bool {
        self.busy.load(Ordering::Relaxed) > 0
    }

    pub fn item_count(&self) -> Result<u32, DaemonError> {
        let conn = self.read.lock().unwrap();
        Ok(conn.query_row("SELECT COUNT(*) FROM items", [], |row| row.get(0))?)
    }

    pub fn db_bytes(&self) -> u64 {
        if let Some(ref path) = self.db_path {
            std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
        } else {
            0
        }
    }

    // ---------------------------------------------------------------- sync core

    pub fn insert(&self, payloads: &PayloadsMap, source_app: &str) -> Result<storage::InsertOutcome, DaemonError> {
        let conn = self.write.lock().unwrap();
        let config = self.config();
        // Pause can be committed while a submission waits for the write connection.
        if config.paused {
            return Ok(storage::InsertOutcome::Skipped(storage::SkipReason::Paused));
        }
        let key = self.history_key();
        storage::insert_protected(&conn, payloads, source_app, &config, key.as_deref())
    }

    pub fn get_history(&self, offset: i64, limit: i64, filter: &str) -> Result<Vec<HistoryRow>, DaemonError> {
        let conn = self.read.lock().unwrap();
        let key = self.history_key();
        storage::list_protected(&conn, offset, limit, filter, key.as_deref())
    }

    pub fn search(&self, query: &str, limit: i64) -> Result<Vec<HistoryRow>, DaemonError> {
        let conn = self.read.lock().unwrap();
        match self.require_history_key()? {
            Some(key) => storage::search_protected(&conn, query, limit, &key),
            None => storage::search(&conn, query, limit),
        }
    }

    pub fn get_item(&self, id: &str) -> Result<Option<ItemMetadata>, DaemonError> {
        let conn = self.read.lock().unwrap();
        let key = self.history_key();
        storage::get_item(&conn, id)?
            .map(|item| storage::restore_metadata(&conn, item, key.as_deref()))
            .transpose()
    }

    pub fn get_content(&self, id: &str, mime: &str) -> Result<ContentRow, DaemonError> {
        let conn = self.read.lock().unwrap();
        let key = self.history_key();
        storage::get_content_protected(&conn, id, mime, key.as_deref())
    }

    pub fn get_payloads(&self, id: &str) -> Result<PayloadsMap, DaemonError> {
        let conn = self.read.lock().unwrap();
        let key = self.history_key();
        storage::get_payloads_protected(&conn, id, key.as_deref())
    }

    pub fn set_pinned(&self, id: &str, pinned: bool) -> Result<bool, DaemonError> {
        let conn = self.write.lock().unwrap();
        storage::set_pinned(&conn, id, pinned)
    }

    pub fn delete_item(&self, id: &str, force: bool) -> Result<bool, DaemonError> {
        let conn = self.write.lock().unwrap();
        storage::delete_item(&conn, id, force)
    }

    pub fn clear_history(&self, include_pinned: bool) -> Result<u32, DaemonError> {
        let conn = self.write.lock().unwrap();
        storage::clear_history(&conn, include_pinned)
    }

    pub fn prune(&self) -> Result<Vec<String>, DaemonError> {
        let conn = self.write.lock().unwrap();
        storage::prune(&conn, &self.config())
    }

    pub fn settings(&self) -> Result<std::collections::BTreeMap<String, config::SettingValue>, DaemonError> {
        Ok(self.config().as_map())
    }

    pub fn set_setting(&self, key: &str, raw: &str) -> Result<(), DaemonError> {
        if key == "encrypt_at_rest" || key == "encryption_key_id" {
            return Err(DaemonError::InvalidArgument("history encryption requires Secret Service".into()));
        }
        let conn = self.write.lock().unwrap();
        config::store_setting(&conn, key, raw)?;
        let mut cfg = self.config.write().unwrap();
        cfg.apply(&key, raw);
        Ok(())
    }

    pub fn add_excluded_app(&self, app_id: &str) -> Result<(), DaemonError> {
        let conn = self.write.lock().unwrap();
        storage::add_excluded_app(&conn, app_id)
    }

    pub fn remove_excluded_app(&self, app_id: &str) -> Result<(), DaemonError> {
        let conn = self.write.lock().unwrap();
        storage::remove_excluded_app(&conn, app_id)
    }

    pub fn list_excluded_apps(&self) -> Result<Vec<String>, DaemonError> {
        let conn = self.read.lock().unwrap();
        storage::list_excluded_apps(&conn)
    }

    pub fn set_mime_rule(&self, mime: &str, store: bool) -> Result<(), DaemonError> {
        let conn = self.write.lock().unwrap();
        storage::set_mime_rule(&conn, mime, store)
    }

    pub fn export(&self, path: &Path, format: &str) -> Result<u32, DaemonError> {
        let conn = self.read.lock().unwrap();
        let key = self.require_history_key()?;
        export::export_protected(&conn, path, format, key.as_deref())
    }

    pub fn import(&self, path: &Path, format: &str) -> Result<u32, DaemonError> {
        let conn = self.write.lock().unwrap();
        let key = self.require_history_key()?;
        export::import_protected(&conn, path, format, key.as_deref())
    }

    pub fn set_has_thumbnail(&self, id: &str) -> Result<(), DaemonError> {
        let conn = self.write.lock().unwrap();
        storage::set_has_thumbnail(&conn, id)
    }

    fn history_key(&self) -> Option<Arc<HistoryKey>> {
        self.history_key.read().unwrap().clone()
    }

    fn require_history_key(&self) -> Result<Option<Arc<HistoryKey>>, DaemonError> {
        if !self.config.read().unwrap().encrypt_at_rest {
            return Ok(None);
        }
        self.history_key().map(Some).ok_or(DaemonError::Encrypted)
    }

    pub(crate) fn cache_history_key(&self, key: Option<Arc<HistoryKey>>) {
        *self.history_key.write().unwrap() = key;
    }

    fn key_id(&self, reserve: bool) -> Result<String, DaemonError> {
        let conn = self.write.lock().unwrap();
        if let Some(id) = conn.query_row(
            "SELECT value FROM settings WHERE key = 'encryption_key_id'", [], |row| row.get(0),
        ).optional()? {
            return Ok(id);
        }
        if !reserve {
            return Err(DaemonError::Encrypted);
        }
        let id = uuid::Uuid::new_v4().to_string();
        config::store_setting(&conn, "encryption_key_id", &id)?;
        Ok(id)
    }

    fn set_encryption(&self, enabled: bool, key: &Arc<HistoryKey>) -> Result<(), DaemonError> {
        let conn = self.write.lock().unwrap();
        // Block this daemon's readers through conversion and the WAL checkpoint.
        let _read = (!Arc::ptr_eq(&self.read, &self.write)).then(|| self.read.lock().unwrap());
        let current = self.config.read().unwrap().encrypt_at_rest;
        if current != enabled {
            conn.pragma_update(None, "secure_delete", true)?;
            storage::convert_history(&conn, enabled, key)?;
            self.cache_history_key(enabled.then(|| key.clone()));
            self.config.write().unwrap().encrypt_at_rest = enabled;
        }

        if enabled && self.db_path.is_some() {
            // Drop pre-encryption free pages and WAL frames from the active database.
            conn.execute_batch("VACUUM")?;
            let busy: i64 = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))?;
            if busy != 0 {
                return Err(DaemonError::Internal("could not truncate the history write-ahead log".into()));
            }
        }
        Ok(())
    }

    pub(crate) async fn key_id_async(&self, reserve: bool) -> Result<String, DaemonError> {
        self.off_reactor(move |db| db.key_id(reserve)).await
    }

    pub(crate) async fn set_encryption_async(&self, enabled: bool, key: Arc<HistoryKey>) -> Result<(), DaemonError> {
        self.off_reactor(move |db| db.set_encryption(enabled, &key)).await
    }

    // -------------------------------------------------- off-reactor wrappers

    async fn off_reactor<T, F>(&self, f: F) -> Result<T, DaemonError>
    where
        T: Send + 'static,
        F: FnOnce(&Database) -> Result<T, DaemonError> + Send + 'static,
    {
        let db = self.clone();
        let busy = self.busy.clone();
        busy.fetch_add(1, Ordering::Relaxed);
        let guard = BusyGuard(busy);
        let joined = tokio::task::spawn_blocking(move || {
            let _guard = guard;
            f(&db)
        }).await;
        match joined {
            Ok(result) => result,
            Err(err) => Err(DaemonError::Internal(format!("storage worker panicked: {err}"))),
        }
    }

    /// Same as `off_reactor`, but does not set the busy flag. `Health`'s own read must
    /// not make the daemon look busy, otherwise the flag would always report true.
    async fn off_reactor_quiet<T, F>(&self, f: F) -> Result<T, DaemonError>
    where
        T: Send + 'static,
        F: FnOnce(&Database) -> Result<T, DaemonError> + Send + 'static,
    {
        let db = self.clone();
        let joined = tokio::task::spawn_blocking(move || f(&db)).await;
        match joined {
            Ok(result) => result,
            Err(err) => Err(DaemonError::Internal(format!("storage worker panicked: {err}"))),
        }
    }

    pub async fn item_count_async(&self) -> Result<u32, DaemonError> {
        self.off_reactor_quiet(move |db| db.item_count()).await
    }

    pub async fn insert_async(&self, payloads: PayloadsMap, source_app: String) -> Result<storage::InsertOutcome, DaemonError> {
        self.off_reactor(move |db| db.insert(&payloads, &source_app)).await
    }

    pub async fn get_history_async(&self, offset: i64, limit: i64, filter: String) -> Result<Vec<HistoryRow>, DaemonError> {
        self.off_reactor(move |db| db.get_history(offset, limit, &filter)).await
    }

    pub async fn search_async(&self, query: String, limit: i64) -> Result<Vec<HistoryRow>, DaemonError> {
        self.off_reactor(move |db| db.search(&query, limit)).await
    }

    pub async fn get_item_async(&self, id: String) -> Result<Option<ItemMetadata>, DaemonError> {
        self.off_reactor(move |db| db.get_item(&id)).await
    }

    pub async fn search_filtered_async(
        &self, query: String, limit: i64, filter: String,
    ) -> Result<Vec<HistoryRow>, DaemonError> {
        self.off_reactor(move |db| {
            let conn = db.read.lock().unwrap();
            let key = db.require_history_key()?;
            storage::search_filtered(&conn, &query, limit, &filter, key.as_deref())
        }).await
    }

    pub async fn touch_async(&self, id: String) -> Result<(), DaemonError> {
        self.off_reactor(move |db| {
            let conn = db.write.lock().unwrap();
            storage::touch(&conn, &id)?;
            Ok(())
        }).await
    }

    pub async fn get_content_async(&self, id: String, mime: String) -> Result<ContentRow, DaemonError> {
        self.off_reactor(move |db| db.get_content(&id, &mime)).await
    }

    pub async fn get_payloads_async(&self, id: String) -> Result<PayloadsMap, DaemonError> {
        self.off_reactor(move |db| db.get_payloads(&id)).await
    }

    pub async fn set_pinned_async(&self, id: String, pinned: bool) -> Result<bool, DaemonError> {
        self.off_reactor(move |db| db.set_pinned(&id, pinned)).await
    }

    pub async fn delete_item_async(&self, id: String, force: bool) -> Result<bool, DaemonError> {
        self.off_reactor(move |db| db.delete_item(&id, force)).await
    }

    pub async fn clear_history_async(&self, include_pinned: bool) -> Result<u32, DaemonError> {
        self.off_reactor(move |db| db.clear_history(include_pinned)).await
    }

    pub async fn set_setting_async(&self, key: String, raw: String) -> Result<(), DaemonError> {
        self.off_reactor(move |db| db.set_setting(&key, &raw)).await
    }

    pub async fn add_excluded_app_async(&self, app_id: String) -> Result<(), DaemonError> {
        self.off_reactor(move |db| db.add_excluded_app(&app_id)).await
    }

    pub async fn remove_excluded_app_async(&self, app_id: String) -> Result<(), DaemonError> {
        self.off_reactor(move |db| db.remove_excluded_app(&app_id)).await
    }

    pub async fn list_excluded_apps_async(&self) -> Result<Vec<String>, DaemonError> {
        self.off_reactor(move |db| db.list_excluded_apps()).await
    }

    pub async fn export_async(&self, path: PathBuf, format: String) -> Result<u32, DaemonError> {
        self.off_reactor(move |db| db.export(&path, &format)).await
    }

    pub async fn import_async(&self, path: PathBuf, format: String) -> Result<u32, DaemonError> {
        self.off_reactor(move |db| db.import(&path, &format)).await
    }
}

#[cfg(test)]
mod encryption_tests {
    use super::*;
    use crate::db::storage::InsertOutcome;

    fn payloads(text: &str) -> PayloadsMap {
        PayloadsMap::from([
            ("text/plain".to_string(), text.as_bytes().to_vec()),
            ("text/html".to_string(), format!("<b>{text}</b>").into_bytes()),
        ])
    }

    #[test]
    fn encryption_migrates_history_and_preserves_restore_after_reopen() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("history.db");
        let db = Database::open_file(&path).unwrap();
        let key = Arc::new(HistoryKey::fixture([17; 32]));
        let marker = "fixture secret preview 7e03f1";
        let payloads = payloads(marker);
        let InsertOutcome::Stored { item, .. } = db.insert(&payloads, "fixture").unwrap() else {
            panic!("fixture was skipped");
        };
        db.set_pinned(&item.id, true).unwrap();
        db.set_encryption(true, &key).unwrap();

        assert_eq!(db.get_payloads(&item.id).unwrap(), payloads);
        assert_eq!(db.search("secret preview", 10).unwrap()[0].0, item.id);
        assert!(db.get_item(&item.id).unwrap().unwrap().pinned);
        let InsertOutcome::Stored { item: duplicate, dedup_hit } = db.insert(&payloads, "fixture").unwrap() else {
            panic!("encrypted duplicate was skipped");
        };
        assert!(dedup_hit);
        assert_eq!(duplicate.id, item.id);

        let raw = std::fs::read(&path).unwrap();
        assert!(!raw.windows(marker.len()).any(|part| part == marker.as_bytes()));
        assert!(!raw.windows(item.content_hash.len()).any(|part| part == item.content_hash.as_bytes()));

        let export = dir.path().join("history.json");
        assert_eq!(db.export(&export, "json").unwrap(), 1);
        let copy = Database::open_in_memory().unwrap();
        copy.set_encryption(true, &key).unwrap();
        assert_eq!(copy.import(&export, "json").unwrap(), 1);
        assert_eq!(copy.get_payloads(&item.id).unwrap(), payloads);
        assert!(storage::get_payloads(&copy.connection().lock().unwrap(), &item.id).is_err());

        drop(db);
        let reopened = Database::open_file(&path).unwrap();
        assert!(reopened.config().encrypt_at_rest);
        assert!(reopened.get_history(0, 10, "all").unwrap()[0].2.contains("keyring locked"));
        assert!(matches!(reopened.get_payloads(&item.id), Err(DaemonError::Encrypted)));
        assert!(matches!(reopened.insert(&payloads, "fixture"), Err(DaemonError::Encrypted)));

        let wrong = Arc::new(HistoryKey::fixture([18; 32]));
        assert!(reopened.set_encryption(false, &wrong).is_err());
        assert!(reopened.config().encrypt_at_rest);
        reopened.cache_history_key(Some(key.clone()));
        assert_eq!(reopened.get_payloads(&item.id).unwrap(), payloads);
        reopened.set_encryption(false, &key).unwrap();
        assert!(!reopened.config().encrypt_at_rest);
        assert_eq!(reopened.get_payloads(&item.id).unwrap(), payloads);
        assert_eq!(reopened.search("secret preview", 10).unwrap()[0].0, item.id);
    }
}

#[cfg(test)]
mod busy_tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn busy_tracks_overlapping_work_after_caller_cancellation() {
        let db = Database::open_in_memory().unwrap();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let slow_db = db.clone();
        let slow = tokio::spawn(async move {
            slow_db.off_reactor(move |_| {
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(())
            }).await
        });
        started_rx.await.unwrap();
        db.get_history_async(0, 10, "all".into()).await.unwrap();
        assert!(db.is_busy(), "the slow worker is still running");
        slow.abort();
        let _ = slow.await;
        assert!(db.is_busy(), "cancellation does not stop blocking work");
        release_tx.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while db.is_busy() {
                tokio::task::yield_now().await;
            }
        }).await.unwrap();
        assert!(db.off_reactor::<(), _>(|_| panic!("test worker panic")).await.is_err());
        assert!(!db.is_busy(), "panic releases the counter");
    }
}

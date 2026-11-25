use crate::config::SettingValue;
use crate::crypto::{keyring_key, HistoryKey};
use crate::db::storage::{InsertOutcome, SkipReason};
use crate::db::Database;
use crate::thumbnails::ThumbnailWorker;
use crate::types::{ApiError, ContentRow, DaemonError, HealthRow, HistoryRow, PayloadsMap};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use futures_lite::StreamExt;
use std::time::Duration;
use tokio::sync::Mutex;
use tracing::info;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{OwnedValue, Value};

pub const DAEMON_VERSION: &str = env!("CARGO_PKG_VERSION");

const KEYRING_TIMEOUT: Duration = Duration::from_secs(5);

struct ActiveKey {
    key: Arc<HistoryKey>,
    item: zbus::Proxy<'static>,
    valid: Arc<AtomicBool>,
    gate: Arc<std::sync::Mutex<()>>,
    _watcher: KeyWatcher,
}

struct KeyWatcher(tokio::task::JoinHandle<()>);

impl Drop for KeyWatcher {
    fn drop(&mut self) {
        self.0.abort();
    }
}

pub struct DaemonService {
    connection: Arc<OnceLock<zbus::Connection>>,
    db: Database,
    thumbnails: ThumbnailWorker,
    pause_updates: Mutex<()>,
    encryption: Mutex<Option<ActiveKey>>,
    ignore_next: Arc<AtomicBool>,
}

impl DaemonService {
    pub fn new(db: Database, thumbnails: ThumbnailWorker) -> Self {
        Self {
            connection: Arc::new(OnceLock::new()),
            db,
            thumbnails,
            pause_updates: Mutex::new(()),
            encryption: Mutex::new(None),
            ignore_next: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn connection_handle(&self) -> Arc<OnceLock<zbus::Connection>> {
        self.connection.clone()
    }

    async fn fetch_key(&self, allow_create: bool) -> Result<ActiveKey, ApiError> {
        let id = self.db.key_id_async(allow_create).await?;
        if self.connection.get().is_none() {
            let bus = zbus::Connection::session().await.map_err(Self::transport)?;
            let _ = self.connection.set(bus);
        }
        let connection = self.connection.get().unwrap();
        // Subscribe before loading the secret so a concurrent lock invalidates the result.
        let rule = zbus::MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .sender("org.freedesktop.secrets").map_err(Self::transport)?
            .build();
        let mut signals = zbus::MessageStream::for_match_rule(rule, connection, Some(64))
            .await.map_err(Self::transport)?;
        let owner = zbus::fdo::DBusProxy::new(connection).await.map_err(Self::transport)?;
        let mut owners = owner.receive_name_owner_changed().await.map_err(Self::transport)?;
        let valid = Arc::new(AtomicBool::new(true));
        let watcher_valid = valid.clone();
        let gate = Arc::new(std::sync::Mutex::new(()));
        let watcher_gate = gate.clone();
        let db = self.db.clone();
        let watcher = tokio::spawn(async move {
            loop {
                let invalidate = tokio::select! {
                    event = signals.next() => match event {
                        Some(Ok(message)) => match message.header().member().map(|member| member.as_str()) {
                            Some("PropertiesChanged") => match message.body().deserialize::<(
                                String, HashMap<String, OwnedValue>, Vec<String>
                            )>() {
                                Ok((_, changed, invalidated)) =>
                                    changed.get("Locked").is_some_and(|value| bool::try_from(value).unwrap_or(true))
                                        || invalidated.iter().any(|name| name == "Locked"),
                                Err(_) => true,
                            },
                            Some("ItemDeleted" | "CollectionDeleted") => true,
                            _ => false,
                        },
                        _ => true,
                    },
                    event = owners.next() => match event {
                        Some(event) => event.args().map_or(true, |args| args.name().as_str() == "org.freedesktop.secrets"),
                        None => true,
                    },
                };
                if invalidate {
                    let _gate = watcher_gate.lock().unwrap();
                    watcher_valid.store(false, Ordering::SeqCst);
                    db.cache_history_key(None);
                    break;
                }
            }
        });
        let watcher = KeyWatcher(watcher);
        let loaded = tokio::time::timeout(KEYRING_TIMEOUT, keyring_key(connection, &id, allow_create))
            .await;
        let (key, item) = match loaded {
            Ok(Ok(loaded)) => loaded,
            _ => {
                return Err(DaemonError::Encrypted.into());
            }
        };
        if !matches!(item.get_property::<bool>("Locked").await, Ok(false)) ||
            !valid.load(Ordering::SeqCst) {
            return Err(DaemonError::Encrypted.into());
        }
        Ok(ActiveKey { key: Arc::new(key), item, valid, gate, _watcher: watcher })
    }

    async fn active_key(&self, active: &mut Option<ActiveKey>) -> Result<Arc<HistoryKey>, ApiError> {
        if let Some(cached) = active {
            let gate = cached.gate.clone();
            let _guard = gate.lock().unwrap();
            if cached.valid.load(Ordering::SeqCst) &&
                matches!(cached.item.cached_property::<bool>("Locked"), Ok(Some(false))) {
                return Ok(cached.key.clone());
            }
            *active = None;
            self.db.cache_history_key(None);
            return Err(DaemonError::Encrypted.into());
        }

        let loaded = self.fetch_key(false).await?;
        let gate = loaded.gate.clone();
        let _guard = gate.lock().unwrap();
        if !loaded.valid.load(Ordering::SeqCst) {
            return Err(DaemonError::Encrypted.into());
        }
        self.db.cache_history_key(Some(loaded.key.clone()));
        let key = loaded.key.clone();
        *active = Some(loaded);
        Ok(key)
    }

    async fn check_keyring(&self) -> Result<(), ApiError> {
        if self.db.config().encrypt_at_rest {
            let mut active = self.encryption.lock().await;
            if self.db.config().encrypt_at_rest {
                self.active_key(&mut active).await?;
            }
        }
        Ok(())
    }

    async fn update_encryption(&self, enabled: bool) -> Result<(), ApiError> {
        let mut active = self.encryption.lock().await;
        let was_enabled = self.db.config().encrypt_at_rest;
        let key = if was_enabled {
            self.active_key(&mut active).await?
        } else if enabled {
            let loaded = self.fetch_key(true).await?;
            let key = loaded.key.clone();
            *active = Some(loaded);
            key
        } else {
            return Ok(());
        };

        let conversion = self.db.set_encryption_async(enabled, key).await;
        if self.db.config().encrypt_at_rest {
            // The data transaction may have committed before checkpoint cleanup failed.
            let cleanup = self.thumbnails.prune_cached().await;
            conversion?;
            cleanup?;
        } else {
            *active = None;
            conversion?;
        }
        Ok(())
    }

    async fn store_setting(&self, key: &str, value: Value<'_>) -> Result<(), ApiError> {
        let raw = match value {
            Value::Bool(b) => if b { "1".to_string() } else { "0".to_string() },
            Value::I16(n) => n.to_string(),
            Value::U16(n) => n.to_string(),
            Value::I32(n) => n.to_string(),
            Value::U32(n) => n.to_string(),
            Value::I64(n) => n.to_string(),
            Value::U64(n) => n.to_string(),
            Value::U8(n) => n.to_string(),
            Value::Str(s) => s.as_str().to_string(),
            _ => return Err(ApiError::InvalidArgument("Expected a boolean, integer, or string".to_string())),
        };
        self.db.set_setting_async(key.to_string(), raw).await.map_err(ApiError::from)?;
        Ok(())
    }

    /// Consumes the one-shot skip flag. True means this event must not be recorded.
    fn should_skip(&self) -> bool {
        if self.db.is_paused() {
            return true;
        }
        self.ignore_next.swap(false, Ordering::SeqCst)
    }

    /// Emitting a signal can fail when the bus is gone. That is a transport failure,
    /// not an API decision, so it is reported as `org.clipboardmanager.Error.Failed`.
    fn transport(err: zbus::Error) -> ApiError {
        ApiError::Failed(err.to_string())
    }

    /// Filesystem and zvariant failures are internal, so they surface as `Failed`.
    fn internal(err: impl std::fmt::Display) -> ApiError {
        ApiError::Failed(err.to_string())
    }
}

#[zbus::interface(name = "org.clipboardmanager.Daemon")]
impl DaemonService {
    /// Ingest a newly copied item with multi-MIME payloads.
    ///
    /// Returns the id of the stored item, or the existing id on a dedup hit. Returns an
    /// empty string when the event is skipped because monitoring is paused or `IgnoreNext`
    /// was armed. Filtered and oversized payloads are reported as errors.
    async fn submit_item(
        &self,
        payloads: PayloadsMap,
        source_app: String,
        #[zbus(signal_emitter)]
        emitter: SignalEmitter<'_>,
    ) -> Result<String, ApiError> {
        if payloads.is_empty() {
            return Err(ApiError::InvalidArgument("payloads are empty".to_string()));
        }
        if self.should_skip() {
            info!("SubmitItem skipped (paused or IgnoreNext)");
            return Ok(String::new());
        }

        self.check_keyring().await?;
        let outcome = self.db.insert_async(payloads, source_app).await?;

        let item = match outcome {
            InsertOutcome::Stored { item, dedup_hit } => {
                if dedup_hit {
                    info!("Dedup hit for item {}", item.id);
                }
                item
            }
            InsertOutcome::Skipped(SkipReason::Paused) => return Ok(String::new()),
            InsertOutcome::Skipped(SkipReason::TooLarge { bytes, limit }) => {
                return Err(ApiError::TooLarge(format!("{bytes} bytes, limit is {limit}")))
            }
            InsertOutcome::Skipped(SkipReason::ExcludedApp(_)) => return Err(ApiError::Filtered("Excluded application".into())),
            InsertOutcome::Skipped(reason) => return Err(ApiError::Filtered(format!("{reason:?}"))),
        };

        self.thumbnails.prune_cached().await.map_err(ApiError::from)?;

        if !item.encrypted && item.primary_mime.starts_with("image/") {
            self.thumbnails.queue(&item.id);
        }

        emitter
            .item_added(item.to_history_row())
            .await
            .map_err(Self::transport)?;
        Ok(item.id)
    }

    /// Retrieve paged history metadata rows.
    async fn get_history(&self, offset: i32, limit: i32, filter: String) -> Result<Vec<HistoryRow>, ApiError> {
        let _ = self.check_keyring().await;
        self.db
            .get_history_async(offset as i64, limit as i64, filter)
            .await
            .map_err(ApiError::from)
    }

    /// Substring & fuzzy search history.
    async fn search_history(&self, query: String, limit: i32) -> Result<Vec<HistoryRow>, ApiError> {
        self.check_keyring().await?;
        self.db
            .search_async(query, limit as i64)
            .await
            .map_err(ApiError::from)
    }

    /// Search within one history type before applying the result limit.
    async fn search_filtered_history(&self, query: String, limit: i32, filter: String) -> Result<Vec<HistoryRow>, ApiError> {
        self.check_keyring().await?;
        self.db.search_filtered_async(query, limit as i64, filter).await.map_err(ApiError::from)
    }

    /// Retrieve metadata for a single item.
    async fn get_item(&self, id: String) -> Result<HistoryRow, ApiError> {
        let _ = self.check_keyring().await;
        let item = self
            .db
            .get_item_async(id.clone())
            .await
            .map_err(ApiError::from)?
            .ok_or_else(|| ApiError::NotFound(format!("item {id} not found")))?;
        Ok(item.to_history_row())
    }

    /// Retrieve binary/text payload for paste-back.
    async fn get_item_content(&self, id: String, mime_type: String) -> Result<ContentRow, ApiError> {
        self.check_keyring().await?;
        let content = self.db.get_content_async(id.clone(), mime_type).await?;
        self.db.touch_async(id).await?;
        Ok(content)
    }

    /// Retrieve all multi-MIME payloads for an item.
    async fn get_item_payloads(&self, id: String) -> Result<PayloadsMap, ApiError> {
        self.check_keyring().await?;
        let payloads = self.db.get_payloads_async(id.clone()).await?;
        self.db.touch_async(id).await?;
        Ok(payloads)
    }

    /// Pre-generated thumbnail PNG, rendered on the worker if it is not cached yet.
    async fn get_thumbnail(&self, id: String) -> Result<Vec<u8>, ApiError> {
        let item = self
            .db
            .get_item_async(id.clone())
            .await
            .map_err(ApiError::from)?
            .ok_or_else(|| ApiError::NotFound(format!("item {id} not found")))?;
        if item.encrypted || !item.primary_mime.starts_with("image/") {
            return Err(ApiError::NotFound(format!("item {id} has no image payload")));
        }

        if let Some(path) = self.thumbnails.cached(&id) {
            return tokio::fs::read(&path).await.map_err(Self::internal);
        }

        let (_, bytes) = self
            .db
            .get_content_async(id.clone(), item.primary_mime.clone())
            .await
            .map_err(ApiError::from)?;
        let path = self.thumbnails.submit(&id, &bytes).await.map_err(ApiError::from)?;
        tokio::fs::read(&path).await.map_err(Self::internal)
    }

    /// Pin an item.
    async fn pin_item(
        &self,
        id: String,
        #[zbus(signal_emitter)]
        emitter: SignalEmitter<'_>,
    ) -> Result<bool, ApiError> {
        self.set_pinned(&id, true, emitter).await
    }

    /// Unpin an item.
    async fn unpin_item(
        &self,
        id: String,
        #[zbus(signal_emitter)]
        emitter: SignalEmitter<'_>,
    ) -> Result<bool, ApiError> {
        self.set_pinned(&id, false, emitter).await
    }

    /// Delete an item. Pinned items are refused unless `force` is set.
    async fn delete_item(
        &self,
        id: String,
        force: bool,
        #[zbus(signal_emitter)]
        emitter: SignalEmitter<'_>,
    ) -> Result<bool, ApiError> {
        let success = self.db.delete_item_async(id.clone(), force).await.map_err(ApiError::from)?;
        if success {
            self.thumbnails.prune_cached().await.map_err(ApiError::from)?;
            emitter.item_removed(id).await.map_err(Self::transport)?;
            emitter.history_changed().await.map_err(Self::transport)?;
        }
        Ok(success)
    }

    /// Clear history.
    async fn clear_history(
        &self,
        include_pinned: bool,
        #[zbus(signal_emitter)]
        emitter: SignalEmitter<'_>,
    ) -> Result<u32, ApiError> {
        let deleted = self.db.clear_history_async(include_pinned).await.map_err(ApiError::from)?;
        if deleted > 0 {
            emitter.history_changed().await.map_err(Self::transport)?;
        }
        self.thumbnails.prune_cached().await.map_err(ApiError::from)?;
        Ok(deleted)
    }

    /// Pause or resume clipboard monitoring.
    async fn set_paused(
        &self,
        paused: bool,
        #[zbus(signal_emitter)]
        emitter: SignalEmitter<'_>,
    ) -> Result<(), ApiError> {
        // Keep concurrent pause requests and their status signals in the same order.
        let _update = self.pause_updates.lock().await;
        let raw = if paused { "1" } else { "0" };
        self.db
            .set_setting_async("paused".to_string(), raw.to_string())
            .await
            .map_err(ApiError::from)?;
        info!("Clipboard monitoring paused: {paused}");
        let state = if paused { "paused" } else { "resumed" };
        emitter
            .status_changed(state.to_string())
            .await
            .map_err(Self::transport)?;
        Ok(())
    }

    /// Skip exactly one upcoming clipboard event (used when the daemon restores the
    /// clipboard itself, so the resulting owner change is not recorded as a new copy).
    async fn ignore_next(&self) -> Result<(), ApiError> {
        self.ignore_next.store(true, Ordering::SeqCst);
        info!("Ignore next clipboard event armed");
        Ok(())
    }

    /// Get all settings.
    async fn get_settings(&self) -> Result<HashMap<String, OwnedValue>, ApiError> {
        let settings = self.db.settings().map_err(ApiError::from)?;
        let mut out = HashMap::new();
        for (key, value) in settings {
            let value = match value {
                SettingValue::Bool(b) => Value::from(b),
                SettingValue::Int(i) => Value::from(i),
                SettingValue::Str(s) => Value::from(s),
            };
            out.insert(key, value.try_to_owned().map_err(Self::internal)?);
        }
        Ok(out)
    }

    /// Set a setting. Values arrive as `v`; the settings table stores TEXT and the type
    /// is inferred from the key, so any numeric or string form is accepted.
    ///
    /// The extension caches settings (it needs `max_item_bytes` before a copy crosses the
    /// bus), so a change is announced with `StatusChanged("settings-changed")`.
    async fn set_setting(
        &self,
        key: String,
        value: Value<'_>,
        #[zbus(signal_emitter)]
        emitter: SignalEmitter<'_>,
    ) -> Result<(), ApiError> {
        if key == "paused" {
            return Err(ApiError::InvalidArgument("Use SetPaused to change monitoring state".to_string()));
        }
        if key == "encrypt_at_rest" {
            let Value::Bool(enabled) = value else {
                return Err(ApiError::InvalidArgument("Expected a boolean for history encryption".into()));
            };
            self.update_encryption(enabled).await?;
        } else {
            self.store_setting(&key, value).await?;
        }
        info!("Settings updated");
        emitter.status_changed("settings-changed".to_string()).await.map_err(Self::transport)?;
        Ok(())
    }

    /// Add an application to the exclusion list.
    ///
    /// The extension filters excluded apps before anything crosses the bus, so it caches
    /// the list and is notified with `StatusChanged("exclusions-changed")`.
    async fn add_excluded_app(
        &self,
        app_id: String,
        #[zbus(signal_emitter)]
        emitter: SignalEmitter<'_>,
    ) -> Result<(), ApiError> {
        self.db.add_excluded_app_async(app_id.clone()).await.map_err(ApiError::from)?;
        info!("Excluded application added");
        emitter
            .status_changed("exclusions-changed".to_string())
            .await
            .map_err(Self::transport)?;
        Ok(())
    }

    /// Remove an application from the exclusion list.
    async fn remove_excluded_app(
        &self,
        app_id: String,
        #[zbus(signal_emitter)]
        emitter: SignalEmitter<'_>,
    ) -> Result<(), ApiError> {
        self.db.remove_excluded_app_async(app_id.clone()).await.map_err(ApiError::from)?;
        info!("Excluded application removed");
        emitter
            .status_changed("exclusions-changed".to_string())
            .await
            .map_err(Self::transport)?;
        Ok(())
    }

    /// The exclusion list, so the extension can filter before anything crosses the bus.
    async fn list_excluded_apps(&self) -> Result<Vec<String>, ApiError> {
        self.db.list_excluded_apps_async().await.map_err(ApiError::from)
    }

    /// Export history.
    async fn export_history(&self, path: String, format: String) -> Result<u32, ApiError> {
        self.check_keyring().await?;
        let count = self.db.export_async(path.into(), format.clone()).await.map_err(ApiError::from)?;
        info!("Exported {count} entries ({format})");
        Ok(count)
    }

    /// Import history.
    async fn import_history(
        &self,
        path: String,
        format: String,
        #[zbus(signal_emitter)]
        emitter: SignalEmitter<'_>,
    ) -> Result<u32, ApiError> {
        self.check_keyring().await?;
        let count = self.db.import_async(path.clone().into(), format.clone()).await.map_err(ApiError::from)?;
        if count > 0 {
            emitter.history_changed().await.map_err(Self::transport)?;
        }
        Ok(count)
    }

    /// Diagnostic health check: (version, entries, db_bytes, busy).
    ///
    /// The count is read off the reactor thread, so `Health` never holds a database lock
    /// on the D-Bus reactor; `busy` therefore describes the work other callers have in
    /// flight, not this call itself.
    async fn health(&self) -> Result<HealthRow, ApiError> {
        let entries = self.db.item_count_async().await.map_err(ApiError::from)?;
        Ok((
            DAEMON_VERSION.to_string(),
            entries,
            self.db.db_bytes(),
            self.db.is_busy(),
        ))
    }

    // Signals
    #[zbus(signal)]
    async fn item_added(emitter: &SignalEmitter<'_>, row: HistoryRow) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn item_removed(emitter: &SignalEmitter<'_>, id: String) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn history_changed(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn status_changed(emitter: &SignalEmitter<'_>, state: String) -> zbus::Result<()>;
}

impl DaemonService {
    async fn set_pinned(&self, id: &str, pinned: bool, emitter: SignalEmitter<'_>) -> Result<bool, ApiError> {
        let success = self.db.set_pinned_async(id.to_string(), pinned).await.map_err(ApiError::from)?;
        if success {
            let _ = self.check_keyring().await;
            let item = self
                .db
                .get_item_async(id.to_string())
                .await
                .map_err(ApiError::from)?
                .ok_or_else(|| ApiError::NotFound(format!("item {id} not found")))?;
            emitter
                .item_added(item.to_history_row())
                .await
                .map_err(Self::transport)?;
        }
        Ok(success)
    }
}

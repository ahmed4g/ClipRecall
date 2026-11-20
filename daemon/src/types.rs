use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum DaemonError {
    #[error("Database error: {0}")]
    Database(#[from] rusqlite::Error),

    #[error("Item not found: {0}")]
    NotFound(String),

    #[error("Payload too large ({0} bytes, limit is {1})")]
    TooLarge(usize, usize),

    #[error("Invalid argument: {0}")]
    InvalidArgument(String),

    #[error("Content rejected: {0}")]
    Filtered(String),

    #[error("Worker pool saturated")]
    Busy,

    #[error("Database locked and no key is available")]
    Encrypted,

    #[error("Internal I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Image error: {0}")]
    Image(#[from] image::ImageError),

    #[error("Internal error: {0}")]
    Internal(String),
}

/// D-Bus error names, as documented in `docs/dbus-api.md`.
///
/// `DaemonError` is the internal error type; this is the wire representation returned
/// from the interface methods. `daemon/tests/dbus_spec.rs` asserts the variant names
/// match the documented error model.
#[derive(Debug, Clone, zbus::DBusError)]
#[zbus(prefix = "org.clipboardmanager.Error")]
pub enum ApiError {
    Failed(String),
    InvalidArgument(String),
    NotFound(String),
    TooLarge(String),
    Filtered(String),
    Busy(String),
    Encrypted(String),
}

impl From<DaemonError> for ApiError {
    fn from(err: DaemonError) -> Self {
        match err {
            DaemonError::InvalidArgument(msg) => ApiError::InvalidArgument(msg),
            DaemonError::NotFound(msg) => ApiError::NotFound(msg),
            DaemonError::TooLarge(bytes, limit) => {
                ApiError::TooLarge(format!("{bytes} bytes, limit is {limit}"))
            }
            DaemonError::Filtered(msg) => ApiError::Filtered(msg),
            DaemonError::Busy => ApiError::Busy("blocking worker pool saturated".to_string()),
            DaemonError::Encrypted => ApiError::Encrypted("database locked, no key available".to_string()),
            other => ApiError::Failed(other.to_string()),
        }
    }
}

/// D-Bus row type matching signature `(ssstbbs)`
/// (id, primary_mime, preview, sort_time, pinned, has_thumbnail, source_app)
pub type HistoryRow = (String, String, String, u64, bool, bool, String);

/// `Health()` reply: (version, entries, db_bytes, busy) -> `(sutb)`
pub type HealthRow = (String, u32, u64, bool);

/// `GetItemContent()` reply: (mime, bytes) -> `(say)`
pub type ContentRow = (String, Vec<u8>);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ItemMetadata {
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
}

impl ItemMetadata {
    pub fn to_history_row(&self) -> HistoryRow {
        (
            self.id.clone(),
            self.primary_mime.clone(),
            self.preview.clone(),
            self.sort_time(),
            self.pinned,
            self.has_thumbnail,
            self.source_app.clone().unwrap_or_default(),
        )
    }

    /// `pinned DESC, sort_time DESC` is the history order; `sort_time` is the value
    /// the row carries.
    pub fn sort_time(&self) -> u64 {
        self.last_used_at.unwrap_or(self.created_at)
    }
}

pub type PayloadsMap = HashMap<String, Vec<u8>>;

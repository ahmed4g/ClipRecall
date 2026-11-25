//! Thumbnail generation worker.
//!
//! Decoding and resizing never happen on the D-Bus reactor thread: jobs are queued on
//! a channel and rendered on tokio's blocking pool. `daemon/tests/thumbnails.rs`
//! asserts that a large job does not delay a D-Bus call.

use crate::db::Database;
use crate::types::DaemonError;
use image::{ImageFormat, ImageReader};
use std::ffi::OsStr;
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};
use tokio::sync::{mpsc, oneshot};
use tracing::{info, warn};

enum Job {
    Render {
        item_id: String,
        bytes: Option<Vec<u8>>,
        reply: Option<oneshot::Sender<Result<PathBuf, DaemonError>>>,
    },
    Prune(oneshot::Sender<Result<(), DaemonError>>),
    /// Graceful drain: the loop finishes the jobs already queued and then stops.
    Stop,
}

/// Handle the service uses to queue jobs. Cheap to clone; the worker loop itself is
/// a separate task, so submitting never blocks the reactor.
#[derive(Clone)]
pub struct ThumbnailWorker {
    tx: mpsc::UnboundedSender<Job>,
    cache_dir: PathBuf,
}

impl ThumbnailWorker {
    /// Creates the handle and starts the worker loop task.
    pub fn new(db: Database, cache_dir: PathBuf) -> (Self, tokio::task::JoinHandle<()>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let worker = Self { tx, cache_dir };
        let join = tokio::spawn(run(rx, db, worker.cache_dir.clone()));
        (worker, join)
    }

    /// Renders one image payload and returns the cached PNG path.
    pub async fn submit(&self, item_id: &str, bytes: &[u8]) -> Result<PathBuf, DaemonError> {
        let (reply, rx) = oneshot::channel();
        self.send(item_id, bytes, Some(reply))?;
        rx.await
            .map_err(|_| DaemonError::Internal("thumbnail worker stopped".to_string()))?
    }

    /// Fire-and-forget for the ingest hot path: the caller must not wait for the render.
    pub fn queue(&self, item_id: &str) {
        if let Err(err) = self.send(item_id, &[], None) {
            warn!("Thumbnail job not queued: {err}");
        }
    }

    fn send(
        &self,
        item_id: &str,
        bytes: &[u8],
        reply: Option<oneshot::Sender<Result<PathBuf, DaemonError>>>,
    ) -> Result<(), DaemonError> {
        if !valid_cache_id(item_id) {
            return Err(DaemonError::InvalidArgument("invalid thumbnail item ID".into()));
        }
        self.tx
            .send(Job::Render {
                item_id: item_id.to_string(),
                bytes: reply.as_ref().map(|_| bytes.to_vec()),
                reply,
            })
            .map_err(|_| DaemonError::Internal("thumbnail worker stopped".to_string()))
    }

    pub fn cached(&self, item_id: &str) -> Option<PathBuf> {
        if !valid_cache_id(item_id) {
            return None;
        }
        let path = self.cache_dir.join(format!("{item_id}.png"));
        path.exists().then_some(path)
    }

    /// Runs after any queued renders, so cleared entries lose their cached images too.
    pub async fn prune_cached(&self) -> Result<(), DaemonError> {
        let (reply, rx) = oneshot::channel();
        self.tx.send(Job::Prune(reply))
            .map_err(|_| DaemonError::Internal("thumbnail worker stopped".to_string()))?;
        rx.await.map_err(|_| DaemonError::Internal("thumbnail worker stopped".to_string()))?
    }

    pub fn cache_dir(&self) -> &PathBuf {
        &self.cache_dir
    }

    /// Stops the loop after the jobs already queued have been rendered.
    pub fn shutdown(&self) {
        let _ = self.tx.send(Job::Stop);
    }
}

async fn run(mut rx: mpsc::UnboundedReceiver<Job>, db: Database, cache_dir: PathBuf) {
    while let Some(job) = rx.recv().await {
        let (item_id, bytes, reply) = match job {
            Job::Render { item_id, bytes, reply } => (item_id, bytes, reply),
            Job::Prune(reply) => {
                let db = db.clone();
                let dir = cache_dir.clone();
                let result = tokio::task::spawn_blocking(move || prune_cached(&db, &dir)).await;
                let _ = reply.send(result.unwrap_or_else(|err| Err(DaemonError::Internal(err.to_string()))));
                continue;
            }
            Job::Stop => break,
        };

        let db = db.clone();
        let dir = cache_dir.clone();

        let joined = tokio::task::spawn_blocking(move || {
            let bytes = match bytes {
                Some(bytes) => bytes,
                None => {
                    let item = db.get_item(&item_id)?
                        .ok_or_else(|| DaemonError::NotFound(format!("item {item_id} not found")))?;
                    db.get_content(&item_id, &item.primary_mime)?.1
                }
            };
            let png = render(&bytes, db.config().thumbnail_max_px.max(1) as u32)?;
            let Some(item) = db.get_item(&item_id)? else {
                return Err(DaemonError::NotFound(format!("item {item_id} not found")));
            };
            if item.encrypted {
                return Err(DaemonError::NotFound("thumbnails are disabled for encrypted history".into()));
            }
            std::fs::create_dir_all(&dir)?;
            let path = dir.join(format!("{item_id}.png"));
            let mut file = tempfile::NamedTempFile::new_in(&dir)?;
            file.write_all(&png)?;
            file.persist(&path).map_err(|err| err.error)?;
            db.set_has_thumbnail(&item_id)?;
            Ok(path)
        })
        .await;

        let result = match joined {
            Ok(r) => r,
            Err(err) => Err(DaemonError::Internal(format!("thumbnail worker panicked: {err}"))),
        };
        // Log the path, never the image bytes.
        match &result {
            Ok(path) => info!("Thumbnail written to {}", path.display()),
            Err(err) => warn!("Thumbnail failed: {err}"),
        }
        if let Some(reply) = reply {
            let _ = reply.send(result);
        }
    }
}

fn prune_cached(db: &Database, cache_dir: &Path) -> Result<(), DaemonError> {
    let entries = match std::fs::read_dir(cache_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err.into()),
    };
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("png") || !entry.file_type()?.is_file() {
            continue;
        }
        let Some(id) = path.file_stem().and_then(|stem| stem.to_str()) else { continue };
        if db.get_item(id)?.is_none_or(|item| item.encrypted) {
            std::fs::remove_file(path)?;
        }
    }
    Ok(())
}

fn valid_cache_id(id: &str) -> bool {
    Path::new(id).file_name() == Some(OsStr::new(id))
}

pub fn render(bytes: &[u8], max_px: u32) -> Result<Vec<u8>, DaemonError> {
    let img = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()?
        .decode()?;
    let resized = img.resize(max_px, max_px, image::imageops::FilterType::Lanczos3);
    let mut out = Vec::new();
    resized.write_to(&mut Cursor::new(&mut out), ImageFormat::Png)?;
    Ok(out)
}

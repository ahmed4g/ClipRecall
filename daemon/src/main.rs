use clipboard_manager_daemon::db::Database;
use clipboard_manager_daemon::service::{DaemonService, DAEMON_VERSION};
use clipboard_manager_daemon::thumbnails::ThumbnailWorker;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::signal::unix::{signal, SignalKind};
use tracing::{info, warn};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

const BUS_NAME: &str = "org.clipboardmanager.Daemon";
const OBJECT_PATH: &str = "/org/clipboardmanager/Daemon";

fn private_dir(path: &Path) -> std::io::Result<()> {
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Initialize logging (never logging clipboard content)
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with(tracing_subscriber::fmt::layer())
        .init();

    info!("Starting clipboard-manager-daemon v{DAEMON_VERSION}");

    // 2. Resolve the database and thumbnail cache inside the user dirs
    let db_path = match dirs::data_local_dir() {
        Some(dir) => {
            let dir = dir.join("clipboard-manager");
            private_dir(&dir)?;
            dir.join("history.db")
        }
        None => PathBuf::from("history.db"),
    };
    let cache_dir = match dirs::cache_dir() {
        Some(dir) => {
            let dir = dir.join("clipboard-manager").join("thumbnails");
            private_dir(&dir)?;
            dir
        }
        None => PathBuf::from("thumbnails"),
    };

    std::fs::OpenOptions::new().write(true).create(true).truncate(false).mode(0o600)
        .open(&db_path)?.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    let db = Database::open_file(&db_path)?;
    info!("Database initialized successfully at {}", db_path.display());

    // 3. Thumbnail worker: decoding and resizing never run on the reactor thread.
    let max_px = db.config().thumbnail_max_px.max(1) as u32;
    let (thumbnails, worker) = ThumbnailWorker::new(db.clone(), cache_dir, max_px);
    let worker_control = thumbnails.clone();
    info!("Thumbnail worker started, cache at {}", thumbnails.cache_dir().display());

    // 4. Register the D-Bus session service. Requesting the name is also the single
    //    instance lock: a second start while the systemd user service is active exits
    //    cleanly instead of serving a second copy of the object.
    info!("Registering D-Bus service at {BUS_NAME}...");
    let conn = match zbus::connection::Builder::session()?
        .name(BUS_NAME)?
        .serve_at(OBJECT_PATH, DaemonService::new(db, thumbnails))?
        .build()
        .await
    {
        Ok(conn) => conn,
        Err(zbus::Error::NameTaken) => {
            info!("{BUS_NAME} is already owned by another instance; exiting.");
            worker_control.shutdown();
            let _ = worker.await;
            return Ok(());
        }
        Err(err) => return Err(err.into()),
    };

    info!("D-Bus service listening on session bus at {OBJECT_PATH}");

    // 5. Run until systemd or the user asks for a clean stop. D-Bus activation means
    //    the daemon is normally terminated with SIGTERM, so that has to drain the
    //    worker and close the database rather than just dropping the process.
    let mut terminate = signal(SignalKind::terminate())?;
    let mut interrupt = signal(SignalKind::interrupt())?;

    tokio::select! {
        _ = terminate.recv() => info!("SIGTERM received"),
        _ = interrupt.recv() => info!("SIGINT received"),
    }

    info!("Shutting down: draining the thumbnail worker");
    worker_control.shutdown();
    if tokio::time::timeout(Duration::from_secs(5), worker).await.is_err() {
        warn!("Thumbnail worker did not drain within 5 s");
    }

    drop(conn);
    info!("Shutdown complete.");

    Ok(())
}

//! Shared harness for the D-Bus integration tests.
//!
//! Every test that talks to the service uses a private bus, so it never touches the
//! user's session bus. `dbus-daemon` is started per test; one connection serves the
//! object and a second connection acts as a client, which is the view a real client has.
//! Tests skip (rather than fail) when `dbus-daemon` is not available.

#![allow(dead_code)]

use clipboard_manager_daemon::db::Database;
use clipboard_manager_daemon::service::DaemonService;
use clipboard_manager_daemon::thumbnails::ThumbnailWorker;
use std::io::{BufRead, BufReader};
use std::process::Child;
use std::time::Duration;
use tempfile::TempDir;

pub const BUS_NAME: &str = "org.clipboardmanager.Daemon";
pub const OBJECT_PATH: &str = "/org/clipboardmanager/Daemon";

/// A client is not allowed to wait longer than this for a reply or a signal.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(10);

pub struct Harness {
    bus: Child,
    pub address: String,
    /// Connection that serves the object.
    pub server: zbus::Connection,
    /// Separate client connection, so the test observes the same ordering a real client would.
    pub client: zbus::Connection,
    // Kept alive so the thumbnail cache directory survives the test.
    dir: TempDir,
    worker: tokio::task::JoinHandle<()>,
}

impl Harness {
    /// Starts a private bus and serves the daemon on it. None means the test should skip.
    pub async fn start(db: Database) -> Option<Harness> {
        let mut bus = match std::process::Command::new("dbus-daemon")
            .args(["--session", "--print-address=1"])
            .spawn()
        {
            Ok(child) => child,
            Err(_) => {
                eprintln!("skipping: dbus-daemon not available");
                return None;
            }
        };

        let mut address = String::new();
        match bus.stdout.as_mut().map(BufReader::new) {
            Some(mut reader) => {
                if reader.read_line(&mut address).is_err() {
                    let _ = bus.kill();
                    return None;
                }
            }
            None => {
                let _ = bus.kill();
                return None;
            }
        }
        let address = address.trim().to_string();
        if address.is_empty() {
            eprintln!("skipping: could not read the private bus address");
            let _ = bus.kill();
            return None;
        }

        let dir = TempDir::new().expect("temp dir");
        let (thumbnails, worker) = ThumbnailWorker::new(db.clone(), dir.path().to_path_buf());

        let server = zbus::connection::Builder::address(address.as_str())
            .expect("bus address")
            .name(BUS_NAME)
            .expect("bus name")
            .serve_at(OBJECT_PATH, DaemonService::new(db, thumbnails))
            .expect("serve object")
            .build()
            .await
            .expect("connect to the private bus");

        let client = zbus::connection::Builder::address(address.as_str())
            .expect("bus address")
            .build()
            .await
            .expect("client connection to the private bus");

        Some(Harness {
            bus,
            address,
            server,
            client,
            dir,
            worker,
        })
    }

    /// Client proxy for the documented interface.
    pub async fn proxy(&self) -> zbus::Proxy<'_> {
        zbus::Proxy::new(&self.client, BUS_NAME, OBJECT_PATH, BUS_NAME)
            .await
            .expect("client proxy")
    }

    pub fn stop(&mut self) {
        let _ = self.bus.kill();
    }
}

/// Calls a method and deserializes the reply.
pub async fn call<B, R>(proxy: &zbus::Proxy<'_>, method: &str, args: &B) -> R
where
    B: serde::ser::Serialize + zbus::zvariant::DynamicType,
    R: for<'d> zbus::zvariant::DynamicDeserialize<'d>,
{
    timeout_call(proxy, method, args, CALL_TIMEOUT).await
}

/// Calls a method that returns nothing, so the reply body is not deserialized.
pub async fn call_void<B>(proxy: &zbus::Proxy<'_>, method: &str, args: &B)
where
    B: serde::ser::Serialize + zbus::zvariant::DynamicType,
{
    timeout_call_void(proxy, method, args, CALL_TIMEOUT).await;
}

pub async fn timeout_call<B, R>(
    proxy: &zbus::Proxy<'_>,
    method: &str,
    args: &B,
    budget: Duration,
) -> R
where
    B: serde::ser::Serialize + zbus::zvariant::DynamicType,
    R: for<'d> zbus::zvariant::DynamicDeserialize<'d>,
{
    tokio::time::timeout(budget, proxy.call(method, args))
        .await
        .unwrap_or_else(|_| panic!("{method} was not answered within {budget:?}"))
        .unwrap_or_else(|err| panic!("{method} failed: {err}"))
}

pub async fn timeout_call_void<B>(
    proxy: &zbus::Proxy<'_>,
    method: &str,
    args: &B,
    budget: Duration,
) -> zbus::Message
where
    B: serde::ser::Serialize + zbus::zvariant::DynamicType,
{
    tokio::time::timeout(budget, proxy.call_method(method, args))
        .await
        .unwrap_or_else(|_| panic!("{method} was not answered within {budget:?}"))
        .unwrap_or_else(|err| panic!("{method} failed: {err}"))
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.stop();
    }
}

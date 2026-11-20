//! Thumbnail worker checks: rendering happens off the reactor, the cached PNG is capped
//! at the configured size, and `has_thumbnail` is set for the item.

use clipboard_manager_daemon::config::Config;
use clipboard_manager_daemon::db::storage;
use clipboard_manager_daemon::db::Database;
use clipboard_manager_daemon::thumbnails::ThumbnailWorker;
use clipboard_manager_daemon::types::PayloadsMap;
use image::{ImageBuffer, Rgb};

fn png_bytes(width: u32, height: u32) -> Vec<u8> {
    let img: ImageBuffer<Rgb<u8>, Vec<u8>> =
        ImageBuffer::from_pixel(width, height, Rgb([240, 120, 60]));
    let mut out = Vec::new();
    img.write_to(
        &mut std::io::Cursor::new(&mut out),
        image::ImageFormat::Png,
    )
    .unwrap();
    out
}

#[tokio::test]
async fn worker_renders_a_capped_png_and_marks_the_item() {
    let db = Database::open_in_memory().expect("in-memory db");
    let dir = tempfile::TempDir::new().unwrap();
    let (worker, join) = ThumbnailWorker::new(db.clone(), dir.path().to_path_buf(), 200);

    let png = png_bytes(640, 480);
    let mut p = PayloadsMap::new();
    p.insert("image/png".to_string(), png.clone());

    let item = {
        let conn = db.connection();
        let conn = conn.lock().unwrap();
        match storage::insert(&conn, &p, "org.gnome.TextEditor", &Config::defaults()) {
            Ok(storage::InsertOutcome::Stored { item, .. }) => item,
            other => panic!("expected a stored image, got {other:?}"),
        }
    };

    assert!(!item.has_thumbnail, "the thumbnail is produced asynchronously");

    let path = worker.submit(&item.id, &png).await.unwrap();
    assert!(path.exists(), "the PNG must be written to the cache dir");
    assert_eq!(path.parent().unwrap(), dir.path());

    let (w, h) = image::image_dimensions(&path).unwrap();
    assert!(w <= 200 && h <= 200, "thumbnail is {w}x{h}, the cap is 200 px");

    let stored = db.get_item(&item.id).unwrap().unwrap();
    assert!(stored.has_thumbnail, "the worker must set has_thumbnail");
    assert_eq!(worker.cached(&item.id), Some(path.clone()));

    db.set_pinned(&item.id, true).unwrap();
    let orphan = dir.path().join("already-deleted.png");
    std::fs::write(&orphan, &png).unwrap();
    assert_eq!(db.clear_history(false).unwrap(), 0);
    worker.prune_cached().await.unwrap();
    assert!(path.exists());
    assert!(!orphan.exists());

    assert_eq!(db.clear_history(true).unwrap(), 1);
    worker.prune_cached().await.unwrap();
    assert!(!path.exists());
    assert!(worker.submit(&item.id, &png).await.is_err());
    assert!(!path.exists(), "a late thumbnail job must not recreate the cleared image");

    drop(worker);
    join.await.unwrap();
}

#[tokio::test]
async fn non_image_payloads_are_rejected_without_crashing_the_worker() {
    let db = Database::open_in_memory().expect("in-memory db");
    let dir = tempfile::TempDir::new().unwrap();
    let (worker, join) = ThumbnailWorker::new(db.clone(), dir.path().to_path_buf(), 200);

    let err = worker.submit("nope", b"not an image").await.unwrap_err();
    println!("rejected as expected: {err}");

    // The worker is still usable afterwards.
    let png = png_bytes(40, 40);
    let payloads = PayloadsMap::from([("image/png".to_string(), png.clone())]);
    let storage::InsertOutcome::Stored { item, .. } = db.insert(&payloads, "test").unwrap() else {
        panic!("image was not stored");
    };
    let path = worker.submit(&item.id, &png).await.unwrap();
    assert!(path.exists());

    drop(worker);
    join.await.unwrap();
}

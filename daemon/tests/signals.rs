//! Every signal in `data/org.clipboardmanager.Daemon.xml` must reach a client that
//! subscribed before the event happened. The service is served on a private bus and the
//! test connects as a separate client, so signals travel through the bus exactly as they
//! do for the extension and the GTK4 app.

mod common;

use clipboard_manager_daemon::db::Database;
use clipboard_manager_daemon::types::{HistoryRow, PayloadsMap};
use futures_lite::stream::StreamExt;
use std::time::Duration;
use zbus::Message;

fn payloads(entries: &[(&str, &[u8])]) -> PayloadsMap {
    entries
        .iter()
        .map(|(m, b)| (m.to_string(), b.to_vec()))
        .collect()
}

async fn next_signal<S>(stream: &mut S, member: &str) -> Message
where
    S: futures_lite::stream::Stream<Item = Message> + Unpin,
{
    let msg = tokio::time::timeout(common::CALL_TIMEOUT, stream.next())
        .await
        .expect("signal did not reach the subscribed client")
        .expect("signal stream ended");
    assert_eq!(
        msg.header().interface().map(|i| i.as_str()),
        Some(common::BUS_NAME),
        "signal came from a different interface"
    );
    assert_eq!(
        msg.header().member().map(|m| m.as_str()),
        Some(member),
        "unexpected signal on the interface"
    );
    msg
}

fn body<T>(msg: &Message) -> T
where
    T: zbus::zvariant::Type + serde::de::DeserializeOwned,
{
    msg.body().deserialize().expect("signal body does not match the spec")
}

/// `HistoryChanged` carries no arguments, so its body is empty.
fn assert_empty_body(msg: &Message) {
    assert_eq!(
        msg.body().signature().to_string(),
        "",
        "HistoryChanged has no arguments"
    );
}

#[tokio::test]
async fn every_documented_signal_reaches_a_subscribed_client() {
    let db = Database::open_in_memory().expect("in-memory db");
    let mut harness = match common::Harness::start(db).await {
        Some(harness) => harness,
        None => return,
    };
    let proxy = harness.proxy().await;

    // Subscribe before triggering anything: a signal emitted before the match rule is
    // added would be missed.
    let mut item_added = Box::pin(
        proxy
            .receive_signal("ItemAdded")
            .await
            .expect("subscribe ItemAdded"),
    );
    let mut item_removed = Box::pin(
        proxy
            .receive_signal("ItemRemoved")
            .await
            .expect("subscribe ItemRemoved"),
    );
    let mut history_changed = Box::pin(
        proxy
            .receive_signal("HistoryChanged")
            .await
            .expect("subscribe HistoryChanged"),
    );
    let mut status_changed = Box::pin(
        proxy
            .receive_signal("StatusChanged")
            .await
            .expect("subscribe StatusChanged"),
    );

    // ItemAdded: a new ingest carries the row.
    let id: String = common::call(
        &proxy,
        "SubmitItem",
        &(payloads(&[("text/plain", b"signal test")]), "signals-test"),
    )
    .await;
    assert!(!id.is_empty(), "SubmitItem returned an empty id");

    let row = body::<HistoryRow>(&next_signal(&mut item_added, "ItemAdded").await);
    assert_eq!(row.0, id, "ItemAdded carries the stored item");

    // ItemAdded again: pinning re-emits the row so the UI can show the pin.
    let pinned: bool = common::call(&proxy, "PinItem", &id).await;
    assert!(pinned);
    let row = body::<HistoryRow>(&next_signal(&mut item_added, "ItemAdded").await);
    assert_eq!(row.0, id);
    assert!(row.4, "the pinned flag is carried in the row");

    // ItemRemoved + HistoryChanged: a delete.
    let deleted: bool = common::call(&proxy, "DeleteItem", &(&id, true)).await;
    assert!(deleted);
    assert_eq!(body::<String>(&next_signal(&mut item_removed, "ItemRemoved").await), id);
    assert_empty_body(&next_signal(&mut history_changed, "HistoryChanged").await);

    // StatusChanged: pause and resume.
    common::call_void(&proxy, "SetPaused", &true).await;
    assert_eq!(body::<String>(&next_signal(&mut status_changed, "StatusChanged").await), "paused");
    common::call_void(&proxy, "SetPaused", &false).await;
    assert_eq!(
        body::<String>(&next_signal(&mut status_changed, "StatusChanged").await),
        "resumed"
    );

    // StatusChanged also invalidates the state the extension caches (settings and the
    // exclusion list), so a change reaches the client that filters before submitting.
    common::call_void(&proxy, "AddExcludedApp", &"signals-test").await;
    assert_eq!(
        body::<String>(&next_signal(&mut status_changed, "StatusChanged").await),
        "exclusions-changed"
    );
    common::call_void(&proxy, "RemoveExcludedApp", &"signals-test").await;
    assert_eq!(
        body::<String>(&next_signal(&mut status_changed, "StatusChanged").await),
        "exclusions-changed"
    );
    common::call_void(
        &proxy,
        "SetSetting",
        &("max_item_bytes", zbus::zvariant::OwnedValue::from(2048u32)),
    )
    .await;
    assert_eq!(
        body::<String>(&next_signal(&mut status_changed, "StatusChanged").await),
        "settings-changed"
    );

    // ImportHistory also reports a changed history.
    let dir = tempfile::TempDir::new().expect("temp dir");
    let path = dir.path().join("import.json");
    std::fs::write(
        &path,
        r#"[{"id":"imported-1","content_hash":"aa","primary_mime":"text/plain","preview":"imported","source_app":"signals-test","created_at":1700000000,"last_used_at":null,"pinned":false,"has_thumbnail":false,"size":8,"encrypted":false}]"#,
    )
    .expect("write import file");
    let count: u32 = common::call(
        &proxy,
        "ImportHistory",
        &(&path.display().to_string(), "json"),
    )
    .await;
    assert_eq!(count, 1);
    assert_empty_body(&next_signal(&mut history_changed, "HistoryChanged").await);

    harness.stop();
}

#[tokio::test]
async fn a_client_that_subscribes_after_an_event_does_not_get_it_replayed() {
    let db = Database::open_in_memory().expect("in-memory db");
    let mut harness = match common::Harness::start(db).await {
        Some(harness) => harness,
        None => return,
    };
    let proxy = harness.proxy().await;

    // The ingest must still work when nobody is listening.
    let id: String = common::call(
        &proxy,
        "SubmitItem",
        &(payloads(&[("text/plain", b"no subscriber")]), "signals-test"),
    )
    .await;
    assert!(!id.is_empty());

    // A later subscriber sees only what happens after it subscribes.
    let late = zbus::connection::Builder::address(harness.address.as_str())
        .expect("bus address")
        .build()
        .await
        .expect("late client connection");
    let late_proxy = zbus::Proxy::new(&late, common::BUS_NAME, common::OBJECT_PATH, common::BUS_NAME)
        .await
        .expect("late proxy");
    let mut stream = Box::pin(
        late_proxy
            .receive_all_signals()
            .await
            .expect("subscribe all signals"),
    );

    assert!(
        tokio::time::timeout(Duration::from_millis(300), stream.next())
            .await
            .is_err(),
        "a client that subscribed after the event received the earlier signal"
    );

    // And it does receive what happens afterwards.
    let id2: String = common::call(
        &proxy,
        "SubmitItem",
        &(payloads(&[("text/plain", b"after subscribing")]), "signals-test"),
    )
    .await;
    let msg = tokio::time::timeout(common::CALL_TIMEOUT, stream.next())
        .await
        .expect("the late subscriber did not receive the later signal")
        .expect("signal stream ended");
    assert_eq!(body::<HistoryRow>(&msg).0, id2);

    harness.stop();
}

#[tokio::test]
async fn filtered_search_and_payload_restore_update_recency() {
    let db = Database::open_in_memory().unwrap();
    let mut harness = match common::Harness::start(db.clone()).await {
        Some(harness) => harness,
        None => return,
    };
    let proxy = harness.proxy().await;
    let id: String = common::call(&proxy, "SubmitItem",
        &(payloads(&[("text/plain", b"needle")]), "test")).await;
    let rows: Vec<HistoryRow> = common::call(&proxy, "SearchFilteredHistory",
        &("needle", 1i32, "text")).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, id);
    let rows: Vec<HistoryRow> = common::call(&proxy, "SearchFilteredHistory",
        &("needle", 1i32, "image")).await;
    assert!(rows.is_empty());
    let conn = db.connection();
    conn.lock().unwrap().execute("UPDATE items SET last_used_at = 1 WHERE id = ?1", [&id]).unwrap();
    let content: (String, Vec<u8>) = common::call(&proxy, "GetItemContent", &(&id, "text/plain")).await;
    assert_eq!(content.1, b"needle");
    assert!(db.get_item(&id).unwrap().unwrap().last_used_at.unwrap() > 1);
    conn.lock().unwrap().execute("UPDATE items SET last_used_at = 1 WHERE id = ?1", [&id]).unwrap();
    let _: PayloadsMap = common::call(&proxy, "GetItemPayloads", &(&id,)).await;
    assert!(db.get_item(&id).unwrap().unwrap().last_used_at.unwrap() > 1);
    harness.stop();
}

//! `data/org.clipboardmanager.Daemon.xml` is the single source of truth for the
//! D-Bus surface. These tests fail if the Rust types used by the service, the
//! generated introspection, or the documented API drift from it.

use clipboard_manager_daemon::db::Database;
use clipboard_manager_daemon::service::DaemonService;
use clipboard_manager_daemon::thumbnails::ThumbnailWorker;
use clipboard_manager_daemon::types::{ContentRow, HealthRow, HistoryRow, PayloadsMap};
use std::path::PathBuf;
use zbus::zvariant::Type;

#[derive(Debug, PartialEq, Eq, Ord, PartialOrd)]
struct Method {
    name: String,
    ins: Vec<String>,
    outs: Vec<String>,
}

#[derive(Debug, PartialEq, Eq, Ord, PartialOrd)]
struct Signal {
    name: String,
    args: Vec<String>,
}

fn attr(segment: &str, key: &str) -> Option<String> {
    let needle = format!("{key}=\"");
    let start = segment.find(&needle)? + needle.len();
    let end = segment[start..].find('"')?;
    Some(segment[start..start + end].to_string())
}

/// Normalise a zvariant signature to its parenthesised struct form.
fn struct_sig(sig: &zbus::zvariant::Signature) -> String {
    let s = sig.to_string();
    if s.starts_with('(') {
        s
    } else {
        format!("({s})")
    }
}

fn parse_node(xml: &str) -> (Vec<Method>, Vec<Signal>) {
    let mut methods = Vec::new();
    for chunk in xml.split("<method name=\"").skip(1) {
        let name = chunk.split('"').next().unwrap().to_string();
        let body = chunk.split("</method>").next().unwrap();
        let mut ins = Vec::new();
        let mut outs = Vec::new();
        for arg in body.split("<arg ") {
            let Some(t) = attr(arg, "type") else { continue };
            match attr(arg, "direction").as_deref() {
                Some("in") => ins.push(t),
                Some("out") => outs.push(t),
                _ => {}
            }
        }
        methods.push(Method { name, ins, outs });
    }

    let mut signals = Vec::new();
    for chunk in xml.split("<signal name=\"").skip(1) {
        let name = chunk.split('"').next().unwrap().to_string();
        let body = chunk.split("</signal>").next().unwrap();
        let args = body
            .split("<arg ")
            .skip(1)
            .filter_map(|a| attr(a, "type"))
            .collect();
        signals.push(Signal { name, args });
    }

    (methods, signals)
}

fn sorted<T: Ord + std::fmt::Debug>(mut v: Vec<T>) -> Vec<T> {
    v.sort();
    v
}

fn spec_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("data")
        .join("org.clipboardmanager.Daemon.xml")
}

/// zbus expands a returned Rust tuple into one out-arg per element, so the XML
/// lists `Health` and `GetItemContent` as separate out args while `GetItem`
/// returns the row as a single struct arg.
fn expected() -> (Vec<Method>, Vec<Signal>) {
    let row = struct_sig(HistoryRow::SIGNATURE);
    let payloads = PayloadsMap::SIGNATURE.to_string();

    let methods = vec![
        Method { name: "SubmitItem".into(), ins: vec![payloads.clone(), "s".into()], outs: vec!["s".into()] },
        Method { name: "GetHistory".into(), ins: vec!["i".into(), "i".into(), "s".into()], outs: vec![format!("a{row}")] },
        Method { name: "SearchFilteredHistory".into(), ins: vec!["s".into(), "i".into(), "s".into()], outs: vec![format!("a{row}")] },
        Method { name: "SearchHistory".into(), ins: vec!["s".into(), "i".into()], outs: vec![format!("a{row}")] },
        Method { name: "GetItem".into(), ins: vec!["s".into()], outs: vec![row.clone()] },
        Method { name: "GetItemContent".into(), ins: vec!["s".into(), "s".into()], outs: vec!["s".into(), "ay".into()] },
        Method { name: "GetItemPayloads".into(), ins: vec!["s".into()], outs: vec![payloads] },
        Method { name: "GetThumbnail".into(), ins: vec!["s".into()], outs: vec!["ay".into()] },
        Method { name: "PinItem".into(), ins: vec!["s".into()], outs: vec!["b".into()] },
        Method { name: "UnpinItem".into(), ins: vec!["s".into()], outs: vec!["b".into()] },
        Method { name: "DeleteItem".into(), ins: vec!["s".into(), "b".into()], outs: vec!["b".into()] },
        Method { name: "ClearHistory".into(), ins: vec!["b".into()], outs: vec!["u".into()] },
        Method { name: "SetPaused".into(), ins: vec!["b".into()], outs: vec![] },
        Method { name: "IgnoreNext".into(), ins: vec![], outs: vec![] },
        Method { name: "GetSettings".into(), ins: vec![], outs: vec!["a{sv}".into()] },
        Method { name: "SetSetting".into(), ins: vec!["s".into(), "v".into()], outs: vec![] },
        Method { name: "AddExcludedApp".into(), ins: vec!["s".into()], outs: vec![] },
        Method { name: "RemoveExcludedApp".into(), ins: vec!["s".into()], outs: vec![] },
        Method { name: "ListExcludedApps".into(), ins: vec![], outs: vec!["as".into()] },
        Method { name: "ExportHistory".into(), ins: vec!["s".into(), "s".into()], outs: vec!["u".into()] },
        Method { name: "ImportHistory".into(), ins: vec!["s".into(), "s".into()], outs: vec!["u".into()] },
        Method { name: "Health".into(), ins: vec![], outs: vec!["s".into(), "u".into(), "t".into(), "b".into()] },
    ];

    let signals = vec![
        Signal { name: "ItemAdded".into(), args: vec![row] },
        Signal { name: "ItemRemoved".into(), args: vec!["s".into()] },
        Signal { name: "HistoryChanged".into(), args: vec![] },
        Signal { name: "StatusChanged".into(), args: vec!["s".into()] },
    ];

    (methods, signals)
}

#[test]
fn spec_xml_matches_the_rust_types() {
    let xml = std::fs::read_to_string(spec_path()).expect("D-Bus spec XML must exist");
    let (methods, signals) = parse_node(&xml);
    let (want_methods, want_signals) = expected();

    assert_eq!(sorted(methods), sorted(want_methods), "method surface drifted from the spec XML");
    assert_eq!(sorted(signals), sorted(want_signals), "signal surface drifted from the spec XML");
}

#[test]
fn rust_type_signatures_are_the_documented_ones() {
    assert_eq!(struct_sig(HistoryRow::SIGNATURE), "(ssstbbs)");
    assert_eq!(struct_sig(HealthRow::SIGNATURE), "(sutb)");
    assert_eq!(struct_sig(ContentRow::SIGNATURE), "(say)");
    assert_eq!(PayloadsMap::SIGNATURE.to_string(), "a{say}");
}

#[test]
fn documented_row_type_matches_the_spec_xml() {
    let docs = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("docs")
        .join("dbus-api.md");
    let docs = std::fs::read_to_string(docs).expect("docs/dbus-api.md must exist");
    let row = struct_sig(HistoryRow::SIGNATURE);

    assert!(docs.contains(&format!("signature: {row}")), "docs/dbus-api.md row signature is stale");
    assert!(!docs.contains("(ssstbbbs)"), "docs/dbus-api.md still contains the old row signature");
}

#[tokio::test]
async fn generated_introspection_matches_the_spec_xml() {
    // Private bus, so the test never touches the user's session bus.
    let mut bus = match std::process::Command::new("dbus-daemon")
        .args(["--session", "--print-address=1"])
        .spawn()
    {
        Ok(child) => child,
        Err(_) => {
            eprintln!("skipping: dbus-daemon not available");
            return;
        }
    };

    let mut address = String::new();
    if let Some(stdout) = bus.stdout.as_mut() {
        use std::io::{BufRead, BufReader};
        let mut reader = BufReader::new(stdout);
        reader.read_line(&mut address).expect("no bus address");
    }
    let address = address.trim().to_string();
    if address.is_empty() {
        eprintln!("skipping: could not start a private bus");
        let _ = bus.kill();
        return;
    }

    let db = Database::open_in_memory().expect("in-memory db");
    let dir = tempfile::TempDir::new().expect("temp dir");
    let (thumbnails, _worker) = ThumbnailWorker::new(db.clone(), dir.path().to_path_buf());
    let service = DaemonService::new(db, thumbnails);
    let conn = zbus::connection::Builder::address(address.as_str())
        .expect("bus address")
        .name("org.clipboardmanager.Daemon")
        .expect("bus name")
        .serve_at("/org/clipboardmanager/Daemon", service)
        .expect("serve object")
        .build()
        .await
        .expect("connect to private bus");

    let proxy = zbus::Proxy::new(
        &conn,
        "org.clipboardmanager.Daemon",
        "/org/clipboardmanager/Daemon",
        "org.freedesktop.DBus.Introspectable",
    )
    .await
    .expect("introspection proxy");

    let generated: String = proxy.call("Introspect", &()).await.expect("Introspect call");

    let _ = bus.kill();

    let (methods, signals) = parse_node(&generated);
    let (want_methods, want_signals) = expected();

    assert_eq!(
        sorted(methods),
        sorted(want_methods),
        "generated introspection differs from data/org.clipboardmanager.Daemon.xml"
    );
    assert_eq!(
        sorted(signals),
        sorted(want_signals),
        "generated signals differ from data/org.clipboardmanager.Daemon.xml"
    );
}

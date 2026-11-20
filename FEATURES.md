# Features

Status legend: `planned` · `unverified` (needs a live GNOME Wayland test) · `done` (tested)

| # | Feature | Component | Status | Notes |
|---|---|---|---|---|
| 1 | Automatic clipboard history capture | extension + daemon | done (text) | Phase 3 verified live capture. A native GTK4 fixture also preserved `text/plain` and `text/html` after its owner exited on 2026-11-06. |
| 2 | Text clipboard support | extension + daemon | planned | UTF-8, multi-line, preview generation |
| 3 | Image clipboard support | extension + daemon | planned | up to 16 MB PNG/JPEG/WebP with thumbnails |
| 4 | File and URI clipboard support | extension + daemon | planned | `x-special/gnome-copied-files`, `text/x-uri` |
| 5 | Instant fuzzy & full-text search | daemon | done | FTS5 trigram index + nucleo fuzzy scoring; tested 2026-11-05, release-mode gate measured |
| 6 | Search filtering (type / app / pinned) | daemon + UI | done (daemon) | text/image/file/pinned/all filters tested 2026-11-06 (`daemon/tests/storage.rs`); filters are mutually exclusive, an unknown filter is `InvalidArgument` and never reaches SQL; UI filter chips pending |
| 7 | Keyboard navigation | UI | done (Up, Down, Escape) | The user verified the Up/Down highlight. Down reached the next page and Escape closed the popup on live Wayland 2026-11-06. Home and End have headless coverage. |
| 8 | Configurable global shortcut | extension | done | `Super+V` opened Clipboard Manager before and after an extension reload once the workstation's notification shortcut moved to `Super+Shift+V`. |
| 9 | Quick select first nine entries | UI | done (digit `1`) | Digit `1` selected the expected row, closed the popup, and restored its bytes on live Wayland 2026-11-06. Badges 1–9 have headless mapping coverage. |
| 10 | Copy selected entry (without pasting) | extension (`set_owner`) | done (quick select) | Quick select closed the popup and restored the exact clipboard bytes on live Wayland 2026-11-06. The dedicated Shift+Enter shortcut remains planned. |
| 11 | Direct paste into the original app | extension (`Clutter.VirtualInputDevice`) | done | `Alt+Enter` verified in Text Editor, Firefox, and Chrome. The Vocalinux listener stayed idle during two consecutive test pastes. The user confirmed the all-apps setting on 2026-11-09. |
| 12 | Paste as plain text | extension | planned | Ctrl+Shift+V strips formatting to text/plain |
| 13 | Pin / unpin entries | daemon + UI | done (Alt shortcut) | `Alt+P` pinned and unpinned a live fixture. The pinned row survived a daemon restart. |
| 14 | Delete individual entries | daemon + UI | done (Alt shortcut) | Live `Alt+D` removed an unpinned row and preserved a pinned row. |
| 15 | Clear unpinned history | daemon + UI | done | Live popup shortcut and panel menu verification confirmed the clear prompt and pinned protection. |
| 16 | Clear all history | daemon + UI | done (daemon) | daemon tested 2026-11-05 (`include_pinned`); UI pending |
| 17 | Ignore selected applications | daemon + extension | done (daemon) | exclusion filter tested 2026-11-05; extension reads `ListExcludedApps` and drops excluded apps before anything crosses the bus (headless tested), app-id source still approximate |
| 18 | Ignore sensitive content & password managers | daemon + extension | done (daemon) | hint MIMEs tested 2026-11-05 on both sides (daemon + `extension/filters.js` headless); entropy rule opt-in (≥ 24 bytes, no whitespace, letters + digits, entropy ≥ 3.5) |
| 19 | Pause / resume clipboard monitoring | extension + daemon | done | Live panel toggle blocked and resumed capture. Pause survived daemon restart and extension recreation. |
| 20 | Ignore next clipboard event | extension | unverified | extension arms the flag and skips the next event locally; daemon-side flag tested 2026-11-05, live path pending |
| 21 | Configurable maximum history size | daemon | done | tested 2026-11-05: `max_entries` window keeps most recent + pinned; default 2000 |
| 22 | Configurable retention period | daemon | done | tested 2026-11-05: age prune never touches pinned; default 30 days |
| 23 | Persistent history after reboot | daemon | unverified after reboot | History and preferences survived an isolated GNOME and session bus restart on 2026-11-09. |
| 24 | Image thumbnails and previews | daemon + UI | done (daemon) | worker tested 2026-11-05: PNG ≤ 200 px, `has_thumbnail` flag, off the reactor; UI display pending |
| 25 | Application source identification | extension | planned | app name & icon from focused window |
| 26 | Configurable appearance & row density | UI | planned | compact rows, custom preview lengths |
| 27 | Light and dark themes | UI | planned | follows GNOME dark/light system preference |
| 28 | Automatic startup | packaging | done (private D-Bus) | Cold activation passed with the staged user installer and the extracted RPM on 2026-11-09. Clean-system systemd activation and reboot are pending. |
| 29 | Import and export history | daemon | planned | JSON / CSV formats |
| 30 | Privacy and security settings | daemon + UI | verified | optional encryption at rest (Secret Service), tested on isolated GNOME Wayland on 2026-11-06 |
| 31 | Multi-MIME clipboard preservation | extension + daemon | planned | preserves rich text (HTML/RTF) + plain text |
| 32 | Popup positioning at mouse cursor or panel | extension UI | done (pointer) | Opens at `global.get_pointer()` with the expected work-area clamp on live Wayland 2026-11-06. Panel placement remains planned. |
| 33 | Top-panel status indicator | extension | done | Live checks verified anchored history, monitoring status and the context menu's clear action. |

## Phase 0 Feasibility Status (2026-11-06)

Verified live on this session:

- [x] `wl-copy` / `wl-paste` round-trip works; multi-MIME targets advertised
  (`UTF8_STRING`, `STRING`, `TEXT`, `text/plain;charset=utf-8`, `text/plain`, `text/html`).
- [x] SQLite FTS5 `trigram` substring matching works (asserted through rusqlite in
  `daemon/tests/storage.rs`).

- [x] The production observer captured text and HTML bytes from a native GTK4 source. Both formats
      matched after the source exited.
- [x] `Main.wm.addKeybinding` fired on `Super+V`. The popup opened at the pointer, took focus,
      and handled navigation and dismissal.
- [ ] `Clutter.VirtualInputDevice.notify_keyval` delivering `Ctrl+V` to a client app.
- [x] Peak daemon RSS was 10.43 MiB with the extension enabled over ten minutes.
- [ ] Idle CPU < 1% over 10 minutes. Whole-shell readings were 11.473% enabled and 6.545% disabled.

The spike extension is installed and enabled but has not been loaded by the shell yet
(`gnome-extensions info clipboard-spike@project.local` reports it is not known; a logout/login
is required). Detailed findings in `docs/phase0-findings.md`.

## Phase 1 Status (2026-11-05)

Verified on this machine:

- [x] Cargo workspace builds clean, no warnings.
- [x] Schema applies cleanly via `PRAGMA user_version`; multi-payload storage and cascade work.
- [x] `busctl call` against the stub daemon answers `Health()` on the live session bus.
- [x] Generated introspection matches `data/org.clipboardmanager.Daemon.xml`
      (`daemon/tests/dbus_spec.rs`, run on a private bus).
- [x] No code path reads clipboard content.

Still open for Phase 1: the extension skeleton has not been loaded in a live session, and
`daemon/tests/` has no coverage of the storage core itself (insert, dedup, prune, pin).

## Phase 2 Status (2026-11-05)

The storage-core test gap from Phase 1 is closed. `cargo test` is green (23 tests, no warnings):

- [x] Insert, dedup hit (bumps `last_used_at`, reorders history), prune, pin, clear — 11 tests in
      `daemon/tests/storage.rs`.
- [x] Trigram and fuzzy search; performance gate measured in release mode: insert mean 268 µs /
      p95 429 µs, worst search 2,465 µs, DB 5.4 MB over 5,000 entries (`docs/testing.md`).
- [x] Concurrency gate: no storage call runs on the D-Bus reactor — a 12 MB ingest in flight while
      `IgnoreNext` (10 ms) and `Health` (100 ms) answer before it finishes; a 5,000-entry search
      does not block the reactor; WAL read works while the write connection is held
      (`daemon/tests/concurrency.rs`).
- [x] Signals gate: `ItemAdded`, `ItemRemoved`, `HistoryChanged`, `StatusChanged` all reach a
      subscribed client on a private bus; a late subscriber is not replayed (`daemon/tests/signals.rs`).
- [x] Privacy gate: clipboard content never appears in logs; insert logs the blake3 hash
      (`daemon/tests/privacy.rs`).
- [x] Thumbnail worker tested (PNG ≤ 200 px, `has_thumbnail` flag, graceful drain).

Still open for Phase 2: live end-to-end capture through the extension (Phase 0 claims 1–4 remain
unverified until the spike extension is loaded), and the search-type/app filter has no test yet.

## Phase 3 Status (written 2026-11-05, acceptance gate measured 2026-11-06)

Ingest bridge written: `extension/filters.js` (pure logic, no `gi://` imports so it runs headless),
`extension/dbus.js` (bridge + marshalling), `extension/extension.js` (observer).

- [x] Filter order: paused → sensitive hints → excluded app → MIME allowlist → debounce →
      `read_async` → fire-and-forget `SubmitItem`.
- [x] 50 ms debounce window; 16 MB size limit read from the daemon's `max_item_bytes` setting.
- [x] Hot path passes `GLib.Bytes` straight into `GVariant('ay')` — zero `Array.from` calls asserted.
- [x] Read path verified against the Meta-18 typelib (`tests/testReadPath.js`, 12/12): the selection is
      `global.display.get_selection()` (`Meta.Selection.get_selection` does not exist), and
      `read_finish()` returns a `Gio.InputStream` rather than `GBytes`, so the payload is read with
      `read_bytes_async`; the dictionary value must be the `GLib.Bytes` itself, not `[bytes]`.
- [x] Reconnect: `g-signal` `StatusChanged` tracks pause state; name appearance is tracked with
      `Gio.bus_watch_name_on_connection` because GLib 2.88.4 has no `g-name-appeared`/`g-name-lost`
      signals on `GDBusProxy` (verified 2026-11-05, `docs/extension-design.md`).
- [x] Cached settings and exclusion list are invalidated by `StatusChanged("settings-changed")` and
      `StatusChanged("exclusions-changed")`, which the daemon now emits from `SetSetting`,
      `AddExcludedApp` and `RemoveExcludedApp`. Without this the extension would keep filtering with a
      stale exclusion list. Asserted in `daemon/tests/signals.rs` and live.
- [x] Proxy built with `new_for_bus_sync`: a proxy created from construct properties never runs
      `g_dbus_proxy_setup()`, so it makes calls but receives no signals (verified 2026-11-05).
- [x] Bridge verified against the live session bus (`gjs -m tests/testBridgeLive.js`, 2026-11-05):
      9/9 — watcher fires, settings + exclusion list read, `SubmitItem` body reaches the daemon and
      writes a row in 1.4 ms at the client, zero `Array.from` calls, and the exclusion change reaches
      the bridge over the live bus.
- [x] `ListExcludedApps` added to the D-Bus surface (XML, service, storage, docs, spec test) so
      excluded apps are dropped before anything crosses the bus.
- [x] Nothing on the ingest path logs clipboard content.
- [x] Headless tests: 21/21 checks (`gjs -m tests/testFilters.js`, `make test-extension`);
      `make pack` now ships `filters.js` and `dbus.js`.

Phase 3 acceptance is **met** (2026-11-06, live GNOME Wayland session, extension ACTIVE,
`python3 tests/verifyObserver.py`): copy → DB row in **30.6 ms**, re-copy bumps the timestamp and
reorders, an excluded app and a `--sensitive` hint copy are rejected end to end, daemon idle CPU
**0.00%** with RSS **11.1 MB**, and the extension's own idle cost is **0.30%** measured as the
gnome-shell delta against the same session with the extension disabled. Numbers in
`docs/testing.md`.

Two bugs the live run found. An unbalanced closer in the read path made the shell reject the module,
and `node --check` (V8) had accepted the same file, so the syntax gate now runs through gjs. And
timestamps had one-second resolution, so two copies inside the same second shared a `sort_time` and
the order between them fell back to insertion order; timestamps are unix milliseconds from schema v2
and history ordering has a `rowid DESC` tie-break.

## Phase 4 Status (live gates measured 2026-11-06)

Popup UI written: `extension/popupLogic.js` (the pure rules the gate asserts) and `extension/popup.js`
(the thin shell wiring), plus `extension/stylesheet.css`.

- [x] Quick select badges 1–9 on the first nine visible rows; the mapping is unit tested.
- [x] Pinned section then recent section, with the separator drawn only when both have rows.
- [x] Keyboard navigation: Up/Down/Home/End, the selection clamped at the ends, Escape closes.
- [x] Opens at `global.get_pointer()`, clamped to the monitor work area so a pointer near an edge does
      not put part of the popup off screen.
- [x] Paged rendering: at most 20 rows, and the next page is fetched only when navigation reaches past
      what is already loaded; a request inside the loaded window does not cross the bus.
- [x] Search debounce 20 ms: a burst of keystrokes produces one query, not one per character.
- [x] `tests/testPopupApi.js` passed 48 checks covering the St/Clutter APIs and popup wiring.
- [x] Selecting a row restores the clipboard owner through `GetItemContent` and suppresses capture
      of the extension's own write. The user pastes with the target application's usual shortcut.

`make test` passed 39 Rust tests and 163 GJS checks. After a fresh login on 2026-11-06 (Qatar local
date), `python3 tests/verifyPopup.py` passed the live gates for opening at the pointer (**2.4 ms**),
typing to search (**2.5 ms** from query to first render), quick select with exact clipboard restore,
second-page fetching, and dismissal by Escape or an outside click. The run used a fresh temporary
history. The original daemon service was restored afterward.

The user confirmed the visible Up/Down highlight. A 5,000-entry compositor trace measured
**8.6–12.1 ms** query-to-render and one skipped refresh slot during typing and settling. Comparison
windows also contained a skipped slot, so the trace did not isolate a cause. The controlled
follow-up passed the zero-drop check with four typing and four matched no-typing windows. Search
rendered in **7.6–11.6 ms**. See
[`docs/testing.md`](docs/testing.md#phase-4-measured-results-2026-11-06) for both runs.

## Phase 5 Status (2026-11-06)

The production manual paste baseline passed in GNOME Text Editor and Firefox after a fresh GNOME
login. The popup restored the exact multilingual fixture with an explicit UTF-8 offer, and Ctrl+V
inserted the exact text in each app. See
[`docs/testing.md`](docs/testing.md#phase-5-manual-paste-measurements-2026-11-06).

## Phase 6 Status (2026-11-06)

The live popup shortcuts passed for pinning, unpinning, deleting and clear confirmation. The
panel opened history below the icon, paused and resumed capture, and opened clear confirmation
from its context menu. A pinned row and paused state survived daemon restart, and the recreated
extension showed paused status. See [`docs/testing.md`](docs/testing.md#phase-6-verification-2026-11-06).

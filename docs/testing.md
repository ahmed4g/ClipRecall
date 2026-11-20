# Testing strategy

## Automated tests

### Rust Daemon (`cargo test`)
- **Storage & Multi-MIME:** Insert multi-payload item (`text/plain` + `text/html`), retrieve payloads, assert deduplication updates `last_used_at` and moves item to top of history.
- **Search Performance:** FTS5 trigram substring matching and in-memory fuzzy scorer (`nucleo-matcher`) tested against synthetic database of 5,000 items. Asserts search completes in < 25 ms.
- **Retention & Pins:** Pruning by maximum entries and age; asserts pinned items are never pruned.
- **Concurrency & Non-blocking I/O:** Concurrent worker pool test verifies that large insertions and thumbnail processing do not block the D-Bus reactor.
- **Privacy Assurance:** Automated test asserts that no raw clipboard string ever appears in `tracing` or `log` outputs.
- **Fixtures:** In-memory SQLite with WAL mode, synthetic text and image payloads (no live session required).

### D-Bus Integration Tests (private bus)

- Private session bus harness (`daemon/tests/common/mod.rs`): `dbus-daemon --session` is started per
  test, the daemon is served on one connection and the test observes it from a separate client
  connection. Tests skip rather than fail when `dbus-daemon` is unavailable.
- `daemon/tests/dbus_spec.rs` — the Rust types, `docs/dbus-api.md`, and the live generated
  introspection must all match `data/org.clipboardmanager.Daemon.xml`.
- `daemon/tests/signals.rs` — every documented signal reaches a client that subscribed before the
  event; a late subscriber is not replayed an earlier signal.
- `daemon/tests/concurrency.rs` — no storage call runs on the D-Bus reactor: a 12 MB ingest in
  flight while `IgnoreNext` (10 ms budget) and `Health` (100 ms budget) answer before it finishes;
  a 5,000-entry search does not block the reactor; a read completes while the write connection is
  held (WAL).

### Extension Unit Tests

Run with `gjs -m tests/testFilters.js` (gjs treats a plain `.js` file as a script, so the `-m`
module flag is required; `make test-extension` runs it). Non-zero exit on failure so the Makefile
gate fails.

- `gjs -m tests/testSyntax.js` parses the extension sources with the engine GNOME Shell actually
  uses (SpiderMonkey). A brace imbalance in `extension/extension.js` was only visible when the
  shell loaded the module on 2026-11-06, and `node --check` (V8) accepted the same file, so the
  check runs through gjs. Imports are stripped and the body is parsed with `new Function()`, which
  parses without executing anything.

- `extension/filters.js` and `extension/popupLogic.js` are pure logic with no `gi://` imports, so they
  run headless without a shell.
- Covers: password manager hint detection, MIME allowlist, 50 ms debounce coalescing (three events
  inside the window produce one flush with the latest value), the 16 MB size limit, and zero-copy
  GVariant marshalling (asserting zero `Array.from` calls on the hot path).
- `gjs -m tests/testPopupLogic.js` (59 checks, 2026-11-06) covers quick select 1–9, the pinned/recent
  sections and the separator between them, the pointer position clamped to the monitor work area,
  keyboard navigation (Up/Down/Home/End, clamped at the ends, shrinking the list keeps the selection
  inside it), the paged render window (a request inside the loaded window must not cross the bus), and
  the 20 ms search debounce (a burst of keystrokes produces one query).
- `gjs -m tests/testPopupApi.js` (48 checks, 2026-11-06) checks the St/Clutter APIs the popup uses
  against the installed typelibs. `Clutter.Actor.add_child` exists and
  `add_actor` does not, `Clutter.Event.get_key_symbol` exists and `get_keyval` does not,
  `text-changed` is a Clutter.Text signal and not an St.Entry signal, St.Entry has no `hint` property
  (the placeholder is `hint-text`), `St.ScrollView` holds one `child`, and `add_style_class_name` is a
  St.Widget method that St.Button inherits. It skips with exit 0 when the St typelib is not reachable,
  so a build root without gnome-shell does not fail.
- Live end-to-end capture through the shell is checked separately in a real GNOME Wayland session
  (see *Live GNOME Wayland Checks*); the headless tests do not prove the observer works.
- `gjs -m tests/testBridgeLive.js` is the live half: it needs the daemon running and skips with
  exit 0 when the bus name has no owner, so it never fails a headless run. It writes one row into
  the real history database.

---

## Phase 2 measured results (2026-11-05, Fedora 44, Rust 1.99.1)

`cargo test` is green: 23 tests (11 storage, 4 D-Bus spec, 3 concurrency, 2 signals, 2 thumbnails,
1 perf, 1 privacy).

Performance gates are measured in **release mode** (`cargo test --release --test perf`); the debug
profile is several times slower and is not what the gate describes, so `daemon/tests/perf.rs`
asserts the documented gates in release and a relaxed budget in debug. Measured on 2026-11-05:

| Metric | Gate | Measured (release) |
|---|---|---|
| Insert latency (5,000 entries) | < 5 ms | mean 268 µs, p95 429 µs |
| Search latency (5,000 entries) | < 25 ms | worst 2,465 µs over 9 queries |
| Database size (5,000 entries) | < 100 MB | 5,435,392 bytes |

Debug-mode run for reference (2026-11-05): insert mean 1,244 µs / p95 2,114 µs, worst search
23,153 µs, DB 5,480,448 bytes — the debug search is close to the gate, which is why the gate is
documented as release-mode measured.

Privacy test: the clipboard string never appears in captured `tracing` output; the insert log line
carries the blake3 hash, not the content.

---

## Phase 3 measured results (2026-11-05)

`cargo test` is green: 38 tests (12 lib including the v2 timestamp migration test, 13 storage,
4 D-Bus spec, 3 concurrency, 2 signals, 2 thumbnails, 1 perf, 1 privacy), no warnings.
Release-mode perf gate re-measured on 2026-11-06: insert mean 453 µs / p95 804 µs over 5,000
entries, worst search 2,606 µs over 9 queries, database 5,558,272 bytes.

Extension headless unit tests (`gjs -m tests/testFilters.js`): 21/21 checks pass — hint detection,
allowlist, size limit, debounce coalescing, zero `Array.from` calls on the marshalling path,
`(a{say}s)` body round-trip, and the `StatusChanged` states that invalidate the cached settings and
exclusion list.

Read path checks (`GI_TYPELIB_PATH=/usr/lib64/mutter-18 gjs -m tests/testReadPath.js`): 12/12 pass.
These found two bugs in the observer before it could run in a live session: `Meta.Selection.get_selection`
does not exist (the selection is reached through `global.display.get_selection()`), and
`read_finish()` returns a `Gio.InputStream`, not `GBytes`, so the payload is read with
`read_bytes_async` before it goes into the `ay` variant. The test also pins that the dictionary value
must be the `GLib.Bytes` itself — `[bytes]` marshals a single byte instead of the payload.

Live session-bus check against the running daemon (`gjs -m tests/testBridgeLive.js`, 2026-11-05):
9/9 pass — the name watcher fires, `GetSettings` and `ListExcludedApps` read from the live daemon,
the marshalled `SubmitItem` body reaches it and writes a row, client → daemon → SQLite in
**1.4 ms** (gate < 100 ms), zero `Array.from` calls on the live path, and
`StatusChanged("exclusions-changed")` invalidates the cached exclusion list. `g-signal` delivered
`StatusChanged('paused')` to a subscribed client. This corrected the bridge twice: GLib 2.88.4
exposes `gConnection`/`gBusType`/`gName`/`gObjectPath`/`gInterfaceName` and only the `g-signal` and
`g-properties-changed` signals, so reconnect uses `Gio.bus_watch_name_on_connection` rather than the
`g-name-appeared`/`g-name-lost` signals, which do not exist here; and a proxy built from construct
properties never receives signals, so the bridge builds it with `new_for_bus_sync`.

Not measured until the extension was loaded in a live session (logout/login required). Run on
2026-11-06 with the extension ACTIVE (`python3 tests/verifyObserver.py`), copies driven with
wl-clipboard as a real Wayland client:

- **A — copy → DB row:** **30.6 ms** after the copy completed (94.5 ms including the wl-copy
  process), gate < 100 ms. The measurement includes the 50 ms debounce.
- **B — re-copy reorders:** passed, but it first exposed a real bug. Timestamps had one-second
  resolution, so two copies inside the same second shared a `sort_time` and the order between them
  fell back to insertion order — a re-copy could not move an item to the top. Timestamps are now
  unix milliseconds (schema v2, with a migration that converts rows written in seconds) and history
  ordering has a `rowid DESC` tie-break. `daemon/tests/storage.rs` has a regression test for the
  same-second reorder, and the v2 conversion is tested in `daemon/src/db/schema.rs`.
- **C — excluded app:** copy rejected. The source app is read from the row gate A produced rather
  than guessed, because the extension records the focused window.
- **D — password manager hint:** `wl-copy --sensitive` with `text/plain` rejected, so the rejection
  is attributable to the hint check rather than to the allowlist.
- **E — idle CPU:** daemon **0.00%** over 240 s, RSS **11.1 MB** (gate < 40 MB). The extension runs
  inside gnome-shell, so its own cost is measured as the difference between gnome-shell with the
  extension enabled and with it disabled: **0.30%** over 60 s windows (5.10% with it, 4.80% without,
  5.05% after re-enabling). The absolute gnome-shell figure includes the 12 other extensions
  enabled in this session and any rendering the session did during the window, so only the delta is
  attributable to the extension, and a 0.30% delta against that noise is not a precise number.

Log review (2026-11-05): `journalctl --user -b 0 /usr/bin/gnome-shell | grep -i clipboard` shows
nothing from the project (the extension is not loaded yet), and no errors attributable to it. The
daemon log lines carry the blake3 hash, never the content — `Stored item f60dea76-6b05-4483-a555-5853209d4daf
(0689781f…, 17 bytes, 1 payloads)` — and the second live run showed the dedup path working on the
live bus (`Dedup hit: item f60dea76-…`). The live check leaves one row in the real history database.

---

## Phase 4 measured results (2026-11-06)

`make test` passed 39 Rust tests and 163 GJS checks. The GJS suite ran 5 syntax, 21 filter, 18 D-Bus,
59 popup logic, 48 popup API, and 12 read path checks.

The popup is shell UI — St actors on `Main.layoutManager.uiGroup` through `addTopChrome`, key focus
through `Main.pushModal` — because GNOME Shell 50 has no layer-shell. The rules the acceptance gate
asserts live in `extension/popupLogic.js` (pure, no `gi://` imports) and `extension/popup.js` is thin
on purpose so those rules stay testable.

Five assumptions were corrected against the typelibs on this machine before the popup could run, and
each is now pinned by `tests/testPopupApi.js`: `add_actor` → `add_child`, `get_keyval` →
`get_key_symbol`, `text-changed` is connected on the entry's Clutter.Text actor, `hint:` →
`set_hint_text`, and the popup root is an `St.BoxLayout` because a plain `St.Widget` has no layout
manager.

Two more rules were corrected by what the live runs measured, rather than by what the code assumed:

- **Quick select reads the text change, not the key event.** `Clutter.Text` consumes printable keys
  before the popup's key handler runs, so a digit pressed with an empty entry arrives as a text change.
  The first live run measured this directly: pressing `1` produced a search for `"1"` instead of
  selecting a row. The rule is now applied to the change itself (`popupLogic.quickSelectFromChange`),
  and the digit branch was removed from the key handler so it no longer claims a path the shell never
  delivers.
- **Navigation counts the reachable row, not only the loaded rows.** Clamping the selection to what the
  pager had fetched made the second page unreachable: the first live run pressed `Down` 20 times on a
  full 20-row window and fetched nothing. `popupLogic.reachableCount` allows the selection to land on the
  row that is not loaded yet, which is exactly the moment the next page is requested.

The restore path now unpacks both `GetItemContent` reply fields and sets the owner for
`Meta.SelectionType.SELECTION_CLIPBOARD`. Each popup open resets navigation before fetching page one.

The gate runs every gate instead of stopping at the first failure, so a failure in one measurement does
not hide the numbers the others could still produce.

`python3 tests/verifyPopup.py` runs the live checks. GNOME Shell caches parsed extension modules, so
run it after a fresh login when those modules change.

- **The keybinding firing**, by injecting `Super+V` through `/dev/uinput` with ydotool. Delivery to the
  compositor was confirmed on 2026-11-06: an injected Super press set `org.gnome.Shell`
  `OverviewActive` to true. So a popup opening after that keypress is the keybinding firing, not a call
  made from outside the shell.
- **Open latency and position** from the extension's own journal line, because an outside process cannot
  time what the shell did inside its own main loop. The gate recomputes the clamp rule in Python and
  compares it with the position the extension used.
- **The key → row mapping end to end**: the clipboard is set to different content, digit `1` is injected
  after the first page has rendered (quick select picks from what is rendered, so pressing it at the
  moment of open would measure a race rather than the rule), and `wl-paste` must return the row quick
  select should have chosen. Key focus is proven by the typed characters producing a search rather than
  going to the window that had focus.

On 2026-11-06 (Qatar local date), the gate ran on Fedora 44 and GNOME 50 Wayland after a fresh login.
The daemon used a fresh temporary history for the fixtures and the original service was restored
after the run.

| Gate | Result | Measurement |
|---|---|---|
| A · Open at pointer | Pass | `Super+V` opened the popup in **2.4 ms**. Position `(649, 458)` matched the clamp rule for pointer `(649, 458)` and work area `(0, 32, 1536, 928)`. |
| B · Search | Pass | Typing in the popup produced the first search render in **2.5 ms** from query to render, with 2 rows. |
| C · Quick select | Pass | Digit `1` selected the expected row, closed the popup, and restored the exact fixture bytes to the clipboard. |
| D · Paging | Pass | The first request rendered 20 rows in **6.3 ms**. After 20 Down presses, the second request rendered 3 rows from offset 20 in **4.8 ms**, for exactly two requests. |
| E · Shell responsiveness | Proxy measured | 15 shell D-Bus round trips with the popup open had a **3.0 ms** median and a **4.5 ms** worst sample. |
| F · Dismissal | Pass | Escape closed the popup. A click at `(587, 396)` also closed it when the popup started at `(649, 458)` and measured `420 × 480`. The harness reopened without moving the pointer to confirm the click position. |

### Compositor trace with 5,000 entries (2026-11-06)

The daemon used 5,000 synthetic entries in temporary history. A continuously repainting GTK4
window supplied frame demand behind the open popup. GNOME Shell recorded a Sysprof trace on the
single active output, `eDP-1` at 60.003 Hz. The already-initialized popup opened in 0.8 ms.
Typing nine characters produced nine search renders in 8.6–12.1 ms from query to render.

During the 674 ms typing and settle window, 40 Clutter presentation timestamps produced 39 intervals.
The median was 16.666 ms and the longest was 33.332 ms, indicating one skipped refresh slot. The
matching KMS flip interval was 33.227 ms. Equal-duration comparison windows before typing and after
search settled also contained a KMS-confirmed skipped slot. This run missed the zero-drop criterion.
The comparison does not establish that clipboard search caused the gap.

The user confirmed in the live popup that Down moved the visible highlight to the next row and Up
moved it back. The quick-select digit check continues to verify the selected row and restored bytes.

### Controlled frame comparison (2026-11-06)

The installed build ran with 5,000 synthetic entries and a continuously animating GTK4 window
behind the popup. A Sysprof trace captured four 900 ms typing windows and four matched no-typing
windows, alternating their order. The single active output was `eDP-1` at 60.003 Hz.

Each condition produced 216 compositor presentations and 212 within-window intervals. All
presentations matched KMS callbacks on CRTC 151. Both conditions had **zero skipped presentation
slots** and a 16.666 ms median interval. The 36 search renders took **7.6–11.6 ms**, with an
8.9 ms median. This controlled run passed the zero-drop check. The earlier isolated gap remains
recorded above.

## Phase 5 manual paste measurements (2026-11-06)

A disposable native GTK4 clipboard source supplied a plain-text fixture and an HTML representation.
The production observer captured them into temporary history. `GetItemPayloads` returned both exact
formats after the source process had exited.

The initial run passed in GNOME Text Editor. Firefox's paste event reported an empty text value
when the extension offered only `text/plain`, although a command-line read returned the exact bytes.
Standard `wl-copy` offers inserted the expected fixture. On this host `wl-copy` advertises several
text aliases even with `--type`. A dedicated GTK4 clipboard owner provided the single-format controls
with the same fixture. Bare `text/plain` reproduced the empty Firefox paste. An offer containing
only `text/plain;charset=utf-8` produced exact Firefox text insertion. GNOME Text Editor saved the
exact bytes of a non-ASCII fixture with the same UTF-8-only offer.

The extension now advertises `text/plain;charset=utf-8` when a restored bare `text/plain` payload
validates as UTF-8. It sends the original bytes and keeps the stored MIME.

After a fresh GNOME login, the production retest selected a multilingual fixture with accented text,
Greek, Japanese, and emoji. Before sending an explicit Ctrl+V, the harness checked for the original
bytes and a sole `text/plain;charset=utf-8` offer in each app.

| Target | Result | Measurement |
|---|---|---|
| GNOME Text Editor 50.1 | Pass | The dedicated editor saved the exact pasted fixture in a temporary scratch file. |
| Firefox 159 | Pass | The test page recorded Ctrl+V, a paste event with the exact text, and an `insertFromPaste` input event with the exact resulting value. |

The popup regression run also passed A, B, C, D, and F. The popup opened in 2.1 ms, and the first
search rendered in 3.7 ms. Gate E measured a D-Bus responsiveness proxy at 3.9 ms median and
7.6 ms maximum. The daemon reopened its usual history after the fixture runs.

On this workstation, Vocalinux uses double Ctrl as a global shortcut. The shared live input helper
rejects both Control keys. For automated paste retests, invoke the target app's Paste menu action
and verify the exact text it inserts.

## Phase 6 verification (2026-11-06)

`make test` passed 40 Rust tests and 222 GJS checks. A private D-Bus run of the release daemon
verified pin and unpin, protected pinned deletion, unpinned deletion, and clearing that retained
the pinned item. Pausing blocked `SubmitItem`. After an actual daemon restart, `GetSettings`
still reported the paused boolean and the pinned row was present. Resuming accepted a new item.

After a fresh GNOME login, the live fixture used temporary history. `Alt+P` pinned and unpinned
the selected item. `Alt+D` deleted an unpinned item and left the pinned item in history.
`Alt+Backspace` opened clear confirmation. Escape preserved both rows and confirmation cleared
only the unpinned row.

Primary click on the panel icon opened history at its anchor. Middle click paused monitoring,
and a live clipboard copy while paused produced no row. Restarting the daemon kept the pause
state and pinned row. Disabling and enabling the extension recreated the panel with paused
status. Middle click resumed monitoring and a new clipboard copy was captured. In a separate
run, secondary click opened the native menu and its clear action opened the anchored confirmation.
Confirming removed only the unpinned item. The daemon reopened the normal history after the runs.

The Shell journal reported an overwritten binding when both Clipboard Manager and GNOME's
notification tray used `Super+V`. On this workstation, the notification tray moved to
`Super+Shift+V` and kept `Super+M` as an alternate. The new notification shortcut took focus
from a GTK test window and Escape returned it. `Super+V` opened Clipboard Manager before and
after an extension reload.

---

## Live GNOME Wayland Checks

Every completed phase is verified on the live Fedora 44 Wayland session:

1. **Copy text:** In GNOME Text Editor → row appears in history within 50 ms.
2. **Copy image:** Using GNOME Screenshot → row appears with generated thumbnail icon.
3. **Copy files:** In Nautilus → URI entry appears; target files are never dereferenced.
4. **Multi-MIME paste:** Copy formatted text from Firefox → paste into LibreOffice retains formatting; paste into GNOME Terminal pastes plain text.
5. **Excluded application:** Copy inside an excluded app (e.g. KeePassXC) → nothing is captured.
6. **Password manager hints:** Emitting `x-kde-passwordManagerHint` or `application/x-gnome-concealed` → copy is dropped immediately.
7. **Global keybinding & cursor placement:** Press `<Super>V` → popup opens directly at mouse cursor with instant keyboard grab (`Main.pushModal`).
8. **Quick select and manual paste:** Select an item with `1`–`9` or Enter, then press `Ctrl+V` in the target app. Compare the pasted text with the fixture.
9. **Copy without paste:** Press `Shift+Enter` → updates clipboard owner, popup closes, no key is injected.
10. **Paste as plain text:** Press `Ctrl+Shift+V` → rich formatting is stripped and plain text is pasted.
11. **Persistence:** Reboot session → database and history survive intact.
12. **Resource consumption:** Measure idle CPU (< 1%) and daemon RSS (< 40 MB) over a 10-minute idle period.

---

## Performance Gates

| Metric | Gate |
|---|---|
| Idle CPU (extension + daemon) | < 1% |
| Daemon memory RSS | < 40 MB |
| Popup open latency (with 5,000 items) | < 50 ms |
| Instant search latency (5,000 items) | < 25 ms |
| Hot path ingest latency (copy → DB) | < 50 ms |
| Compositor frame drop during ingest | 0 dropped frames |

### Ten-minute idle measurements (2026-11-06)

The sampler read process user and system CPU ticks over 600 seconds and sampled daemon RSS every
five seconds. CPU percentages use one core as the denominator. GNOME's idle inhibitor kept the
session awake during the sample. The normal history daemon was active.

| Extension | GNOME Shell CPU | Daemon CPU | Peak daemon RSS |
|---|---|---|---|
| Enabled | 11.473% | 0.000% | 10.43 MiB |
| Disabled | 6.545% | 0.000% | 10.16 MiB |

Both windows had zero logged clipboard activity. The enabled sample passed the 40 MiB RSS limit.
The CPU readings differed by 4.928 percentage points and did not establish the under-1% extension
budget. These sequential samples include unrelated Shell work, so the difference is not a direct
measurement of the extension. The extension was re-enabled after the comparison.

In the controlled follow-up, a static maximized GTK4 window stayed in front for the full enabled
sample. GNOME Shell used **1.177%** of one core, the daemon used **0.000%**, and daemon RSS peaked
at **10.32 MiB**. There was no logged clipboard activity. The disabled sample stopped when the
window lost focus. The extension was re-enabled, and the matched CPU attribution remains open.

---

## Log Review

After each phase, run:
```bash
journalctl --user -u clipboard-manager-daemon.service -b 0
journalctl --user -b 0 /usr/bin/gnome-shell | grep -i clipboard
```
Verify zero errors and verify that no clipboard content text ever appears in system logs.

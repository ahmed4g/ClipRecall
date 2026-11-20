# Architecture

## Overview

```
+-------------------------------------------------------------------------+
|                      GNOME Shell Extension (GJS)                        |
|                                                                         |
|  - Clipboard observer (Meta.Selection owner-changed)                    |
|  - Global keybinding (Meta.Display.add_keybinding)                      |
|  - Top-panel indicator & status menu                                    |
|  - Mouse-cursor positioned popup (St.Widget / PopupMenu + modal grab)   |
|  - Auto-paste engine (Clutter.VirtualInputDevice notify_keyval)         |
|  - Preferences UI (prefs.js with Adw.PreferencesWindow)                 |
+-------------------------------------------------------------------------+
                                    |
                 session D-Bus      | SubmitItem(multi-mime, bytes, app_id)
                 (org.clipboard     | SearchHistory / GetHistory / GetItemContent
                  manager.Daemon)   |
                                    v
+-------------------------------------------------------------------------+
|                  clipboard-manager-daemon (Rust)                        |
|                                                                         |
|  - Tokio async reactor + zbus session service                           |
|  - SQLite (WAL mode, PRAGMA synchronous = NORMAL)                       |
|  - FTS5 trigram index + in-memory fuzzy scoring (nucleo/sublime)        |
|  - blake3 deduplication & multi-MIME payload store                      |
|  - Thumbnail generation pool (PNG ~200px)                               |
|  - Retention enforcement (max items, max age, pin rules)                |
|  - Optional Secret Service encryption at rest                           |
+-------------------------------------------------------------------------+
                                    |
                                    v
            ~/.local/share/clipboard-manager/ (db)
            ~/.cache/clipboard-manager/thumbnails/ (thumbs)
```

The system uses a two-tier architecture tailored to GNOME Wayland:

1. **GNOME Shell Extension (GJS)** — Runs inside the compositor process (`gnome-shell`). It has privileged access to observe clipboard ownership changes, position popups at the exact cursor coordinates (`global.get_pointer()`), manage instant keyboard focus (`Main.pushModal()`), and synthesize keystrokes directly to client windows for automatic paste.
2. **Rust Daemon** — Runs out-of-process as a systemd user service. Handles all SQLite transactions, indexing, fuzzy scoring, multi-MIME persistence, thumbnail rendering, and encryption off the compositor thread.
3. **Preferences (`prefs.js`)** — Native libadwaita preferences window loaded by GNOME Extensions (`gnome-extensions prefs`), communicating with the daemon over D-Bus.

---

## Component A — GNOME Shell Extension (GJS)

### Responsibilities

1. **Observe Clipboard:** Connects to `Meta.Selection.connect('owner-changed')`.
2. **Filter & Ingest:** Checks paused state, exclusion list, password manager suppression hints (`x-kde-passwordManagerHint`, `application/x-gnome-concealed`), and reads available MIME formats.
3. **Non-blocking Ingest:** Uses `Meta.SelectionSource.read_async` and sends byte buffers to the daemon using `GLib.Variant('ay', bytes)` / `GLib.Bytes`. **Strictly forbidden: `Array.from(bytes)`**, which would freeze SpiderMonkey garbage collection.
4. **Global Keybinding:** Registered via `global.display.add_keybinding` using the extension's `Gio.Settings`.
5. **Popup UI:** Displays a sleek, compact Maccy-style popup anchored directly at the mouse cursor or top bar indicator. Grabs keyboard instantly via `Main.pushModal()`.
6. **Automatic Paste:** Restores the clipboard owner via `Meta.SelectionSourceMemory`, closes the modal, and synthesizes `Ctrl+V` (or `Shift+Insert` for terminals) into the previously active window using `Clutter.VirtualInputDevice`.

### Hot Path Performance Rules

- Zero heavy computation on the compositor thread: no image scaling, no hashing, no database queries.
- Zero copy buffer transfer: pass binary buffers directly into GVariant byte arrays (`ay`).
- 50 ms debounce per selection event to coalesce multi-event copies from complex apps.

---

## Component B — Rust Daemon (`clipboard-manager-daemon`)

### Responsibilities

- **D-Bus Reactor:** Built with `zbus` on the user session bus (`org.clipboardmanager.Daemon`).
- **Database Threading:** All SQLite operations execute in `tokio::task::spawn_blocking` behind a connection mutex so D-Bus responsiveness is never blocked by disk I/O.
- **Deduplication:** Computes `blake3` hash across item content. When an identical item is copied, it updates `last_used_at` and moves the item to the top without duplicating data.
- **Multi-MIME Storage:** Stores both plain text (`text/plain`) and rich content (`text/html`, `text/rtf`, images) in `item_payloads` so pasting into rich text editors preserves formatting, while terminal pastes remain clean.
- **Search Engine:**
  - SQLite FTS5 table with `tokenize = 'trigram'` for instant substring matching across code, URLs, and filenames.
  - In-memory scoring over recent items for Maccy-style fuzzy matching (e.g. `gdoc` matches `Google Document`).
- **Thumbnails:** Background worker generates 200px PNG thumbnails for image payloads, written directly to `~/.cache/clipboard-manager/thumbnails/<id>.png`.
- **Retention:** Prunes expired or excess unpinned items (default 2,000 items, 30-day retention). Pinned items are protected.

---

## Data Flows

### Ingest (Hot Path)
```
User copies in application
  -> Mutter emits Meta.Selection 'owner-changed'
  -> Extension: filter (exclusion list + password manager hints)
  -> Extension: async read_async for allowed MIME types (non-blocking)
  -> D-Bus fire-and-forget: SubmitItem(multi_payloads, source_app)
  -> Daemon: blake3 hash, dedup check, SQLite insert, thumbnail generation
  -> Daemon emits ItemAdded signal
```

### Search & Select
```
User presses shortcut (e.g. Super+V)
  -> Extension: captures previous focused window ID
  -> Extension: opens popup at global.get_pointer(), calls Main.pushModal()
  -> Extension queries SearchHistory(query, limit=15) over D-Bus
  -> Daemon returns lightweight row metadata only (id, preview, mime, timestamp, pinned)
  -> Popup renders visible rows (< 1ms in Clutter)
```

### Automatic Paste
```
User hits Enter or quick-select key (1-9)
  -> Extension fetches item content via GetItemContent(id)
  -> Extension sets clipboard owner (Meta.SelectionSourceMemory + set_owner)
  -> Extension closes popup modal and restores focus to previous window
  -> Extension emits Ctrl+V via Clutter.VirtualInputDevice.notify_keyval()
  -> Active application receives paste instantly
```

*Note: Pressing Shift+Enter copies the item to the clipboard without auto-pasting. Pressing Ctrl+Shift+V normalizes formatting to `text/plain` before pasting.*

---

## Threading & Performance Model

| Task | Execution Location | Guarantee |
|---|---|---|
| Clipboard Observation | Compositor thread (GJS) | Signal handler only, non-blocking |
| Payload Ingest | GJS async IO | Returns immediately to Mutter render frame |
| Hash / Dedup / SQLite / Thumbnails | Daemon thread pool | Off the compositor, concurrent with UI |
| Search | Daemon (trigram FTS5 + fuzzy scorer) | O(log n), sub-5ms response |
| Popup Display & Keyboard Grab | GNOME Shell (`St.Widget` / Clutter) | Native actor, no XDG activation latency |
| Preferences & Heavy Configuration | `prefs.js` (Adw.PreferencesWindow) | Separate settings process |

**Target Budgets:** Idle CPU < 1%, Daemon RSS < 40 MB, Popup open latency < 50 ms, Search latency over 5,000 entries < 25 ms.

---

## Known Boundaries & Realities

1. **Compositor Integration:** Mutter does not implement `wlr-data-control`, so a GNOME Shell extension is strictly necessary.
2. **Wayland Window Coordinates:** Standalone Wayland client windows cannot self-position at the mouse cursor; therefore the popup is implemented as a Shell-native actor rather than an external GTK4 window.
3. **Application Identification:** `Meta.SelectionSource` does not identify the source client; the extension records the currently focused window (`global.display.get_focus_window()`) at the moment of copy.
4. **Password Managers:** Password managers on Linux emit hints like `x-kde-passwordManagerHint` and `application/x-gnome-concealed`. The extension filters these deterministically; Shannon entropy heuristics are opt-in to avoid dropping developer hashes and tokens.

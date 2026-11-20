# Implementation plan

Each phase ends with: compile, automated tests, log review, a real GNOME Wayland check,
documentation update, and a Git commit. A phase is not considered done until its
acceptance tests pass. No placeholder UI, no fake functionality.

## Proposed repository layout

```
clipboard/
  README.md  plan.md  ARCHITECTURE.md  FEATURES.md  TODO.md
  docs/                      # architecture and technical specs
  daemon/                    # Rust crate: storage, search, D-Bus service, CLI
    Cargo.toml  src/  tests/
  extension/                 # GJS GNOME Shell extension
    metadata.json  extension.js  popup.js  indicator.js  prefs.js
    schemas/       stylesheet.css  tests/
  packaging/
    clipboard-manager.spec
    clipboard-manager-daemon.service
    org.clipboardmanager.Daemon.service
  Makefile                   # build, test, and install automation
```

A root `Makefile` orchestrates compiling the Rust daemon, compiling schemas, packing the extension, and running tests.

## Dependencies to install before Phase 1

```bash
sudo dnf install -y gtk4-devel libadwaita-devel sqlite-devel gjs-devel meson ninja make
```

Rust crates (fetched from crates.io):
`zbus`, `tokio`, `rusqlite`, `blake3`, `image`, `uuid`, `serde_json`, `tracing`, `tracing-subscriber`,
`secret-service`, `nucleo-matcher` (for Maccy fuzzy search scoring).

## Phase 0 — Feasibility spike (no product code)

Purpose: prove the core Wayland assumptions on this machine before committing to product code.

Tests:

1. **Observation & Multi-MIME Read** — a minimal extension registers `owner-changed` and logs
   available MIME types (`text/plain`, `text/html`, images) for copies made from real apps
   (GNOME Text Editor, Firefox, GNOME Screenshot).
   *Pass:* every copy produces an event with expected MIME types; content persists after source exits.
2. **Keybinding** — the product registers the `Gio.Settings` accelerator through
   `Main.wm.addKeybinding`. The spike's direct `global.display.add_keybinding` registration was
   filtered out by GNOME Shell.
   *Pass:* keybinding callback fires reliably.
3. **Automatic Paste Experiment via Clutter Virtual Device** — extension instantiates
   `Clutter.VirtualInputDevice(KEYBOARD_DEVICE)` and calls `notify_keyval` to emit `Ctrl+V` into a target app.
   *Pass:* target app receives and pastes the content without manual keyboard intervention.
4. **Mouse-Positioned Popup & Modal Focus** — a minimal `St.Widget` / `PopupMenu` opens at `global.get_pointer()`
   and grabs keyboard focus via `Main.pushModal()`.
   *Pass:* popup opens at mouse coordinates, handles arrow keys, and closes cleanly on Escape or outside click.
5. **Idle cost** — extension idle CPU measured for 10 minutes.
   *Pass:* < 1% idle CPU.

Deliverable: `docs/phase0-findings.md`, and the plan updated with the measured results.

## Phase 1 — Skeleton, schema, D-Bus spec

- Git repo, Cargo workspace, extension skeleton, packaging skeleton.
- `docs/db-schema.sql` applied by the daemon with `PRAGMA user_version` migrations, supporting multi-payload storage.
- D-Bus interface XML as the single source of truth (`org.clipboardmanager.Daemon`).
- FEATURES.md / TODO.md / ARCHITECTURE.md committed.

*Acceptance:* schema applies cleanly; `busctl call` against a stub daemon answers
`Health()`; no code path touches clipboard content yet.

## Phase 2 — Daemon storage core

- SQLite (WAL mode, `PRAGMA synchronous = NORMAL`), FTS5 trigram index (`tokenize = 'trigram'`).
- `blake3` dedup with `last_used_at` timestamp bump, retention (max entries + max age), pin rules,
  MIME rules, app exclusion table, preview computation (<= 200 chars).
- In-memory fuzzy matcher (`nucleo-matcher`) for Maccy-style instant search.
- Thumbnail generation worker for image payloads up to 16 MB.

*Acceptance:* `cargo test` covers insert, dedup hit, prune, pin, clear, trigram and fuzzy search.
5,000 synthetic entries: search < 25 ms, insert < 5 ms, DB < 100 MB.
No blocking call on the D-Bus reactor (asserted by a concurrency test).

## Phase 3 — D-Bus service + extension bridge

- Full method/signal surface from `docs/dbus-api.md`.
- Extension ingest path: filter (paused, exclusion, password manager hints) → `read_async` →
  fire-and-forget `SubmitItem` passing `GLib.Bytes` directly into `GVariant('ay')` (no `Array.from`).
- 50 ms debounce, 16 MB size limit.

*Acceptance:* copy in a real app → row appears in the DB within 100 ms;
re-copying updates timestamp and moves item to top; excluded apps and password manager copies are rejected;
idle CPU stays < 1%.

## Phase 4 — Popup UI & Instant Search

- Shell-native popup (`St.Widget` / `PopupMenu`) anchored at mouse pointer (`global.get_pointer()`)
  or top-bar indicator.
- Search box focused on open, pinned and recent sections with visual separator.
- Full keyboard navigation (Up/Down, Home/End, Esc to close).
- Quick select `1`–`9` badges.
- Paged rendering (< 20 visible items queried from daemon on demand).

### Acceptance checks

- The popup opens at the cursor in < 50 ms and typing performs instant trigram/fuzzy search.
- Direct compositor measurement shows zero dropped frames while typing to search.
- A quick-select digit chooses the row with the matching badge, dismisses the popup immediately,
  and restores the row's exact bytes to the clipboard.
- Up and Down move the visible selection highlight to the expected row in the open popup.

## Phase 5 — Clipboard Restore and Paste Back

Selecting an item with Enter or `1`–`9` dismisses the popup, returns focus to the target window, and
restores the payload with `Meta.Selection.set_owner`. The user then presses `Ctrl+V`.

**Acceptance** requires selecting a plain-text item and manually pasting it with `Ctrl+V` into both
GNOME Text Editor and Firefox. The pasted text must match the fixture byte for byte in each app.

**Follow-up** covers additional clients and formats, including Nautilus, an XWayland app, and
rich-text paste. Dedicated `Shift+Enter` copy and `Ctrl+Shift+V` plain-text paste are separate
shortcuts. Test synthesized `Ctrl+V` with `Clutter.VirtualInputDevice` as a separate experiment.

## Phase 6 — Item actions & Top-panel Indicator

- Top-bar indicator with active and paused status. Primary click opens history below the icon.
  The context menu offers pause or resume and clear unpinned. Preferences arrives in Phase 8.
- Popup buttons and `Alt+P` pin or unpin, `Alt+D` deletes an unpinned item, and `Alt+Backspace`
  opens clear-unpinned confirmation. Unmodified letters and Backspace continue editing search.

*Acceptance:* verify each action against live history. Pinned items survive pruning and clearing
unpinned history. Pins and monitoring state persist across shell and daemon restarts.

## Phase 7 — Images, files, and Multi-MIME rich text

- Image entries stored as blobs, thumbnails generated at ingest (~200px PNG in cache), rendered in popup.
- Multi-MIME retention: copying formatted text from a browser preserves both `text/html` and `text/plain`.
- File/URI entries (`x-special/gnome-copied-files`, `text/x-uri`) stored as references, never dereferenced.

*Acceptance:* pasting formatted web content into LibreOffice preserves formatting, while pasting into
GNOME Terminal pastes plain text; images up to 16 MB render thumbnails smoothly without compositor hitch.

## Phase 8 — Preferences window

- `prefs.js` using `Adw.PreferencesWindow` loaded via `gnome-extensions prefs`.
- General (keybinding, history size, retention days), Appearance (compact rows, thumbnail sizes),
  Privacy (password manager hints, app exclusions, encryption toggle), Storage (clear data, export).
- Directly configures daemon settings and GSettings over D-Bus.

*Acceptance:* every setting persists, applies live without shell restart where possible, and is
covered by tests.

## Phase 9 — Security hardening & Privacy

- Secret Service key storage, optional AES-256-GCM encryption at rest.
- Password manager detection test (`x-kde-passwordManagerHint`, `application/x-gnome-concealed`).
- Automated no-content-in-logs test.

*Acceptance:* verified that password manager copies never hit the database; no clipboard text
appears in logs; encryption round-trip passes.

## Phase 10 — Direct paste, packaging, and autostart

- Add **Alt+Enter** in the history popup to restore the selected item and paste it into the app that
  was focused before the popup opened. **Enter** and **1–9** restore the item to the clipboard.
- Use GNOME Shell’s virtual keyboard after releasing the popup grab and waiting for Alt to be
  released. Send one paste shortcut only if the original window has focus and the restored item
  still owns the clipboard. If focus changes, leave the restored item available for manual paste.
- RPM package spec: daemon binary, extension files, systemd user unit, D-Bus activation service,
  GSettings schema.
- Local user installation: `make install-user` installs directly to `~/.local/` and enables the extension.

*Acceptance:* verify that **Alt+Enter** inserts the selected text into GNOME Text Editor and Firefox
on Wayland, starting in an isolated GNOME session. Verify that a focus change cancels direct paste
and that two quick pastes with Vocalinux running leave its double-Ctrl dictation shortcut idle.
On a clean Fedora 44 install, verify extension enablement, on-demand D-Bus activation, and
persistence across a reboot.

## Definition of done

A feature is marked done in FEATURES.md only when:

- code exists,
- automated tests pass,
- it was checked on this GNOME Wayland session, with the date recorded,
- documentation reflects reality.

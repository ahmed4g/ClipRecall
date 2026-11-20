# GNOME Shell extension design

UUID: `clipboard-manager@project.local`
Target: GNOME Shell 50 (gjs 1.88). ESModule extension.

## Files

```
extension/
  metadata.json
  extension.js            # entry point, lifecycle, observer, keybinding, auto-paste
  popup.js                # Shell-native popup (St.Widget, cursor positioning, keyboard navigation)
  indicator.js            # top-panel status indicator icon & context menu
  dbus.js                 # session bus proxy, typed calls, reconnect logic
  filters.js              # MIME allowlist, password manager hints, app exclusion
  prefs.js                # libadwaita preferences window (Adw.PreferencesWindow)
  schemas/org.clipboardmanager.gschema.xml
  stylesheet.css          # theme styling for popup and badges
```

## Lifecycle

`enable()`

1. Read settings (`org.clipboardmanager`).
2. Connect to `org.clipboardmanager.Daemon` via `Gio.DBusSessionBusConnection`
   (`GDBusProxy` with `G_BUS_TYPE_SESSION`). D-Bus activation starts the daemon on demand.
3. Add top-panel status indicator (`indicator.js`).
4. Register global keybinding via `global.display.add_keybinding(...)`.
5. Connect `owner-changed` on `global.display.get_selection()`.

`disable()`

- Disconnect all signals, remove keybinding via `global.display.remove_keybinding()`,
  destroy indicator, release modal grabs, and drop D-Bus proxy.

### GDBusProxy facts verified on gjs 1.88 / GLib 2.88.4 (2026-11-05)

- Construct properties are `gConnection`, `gBusType`, `gName`, `gObjectPath`,
  `gInterfaceName`. The older `name` / `objectPath` / `interfaceName` spellings raise
  `No property name on GDBusProxy`, so `extension/dbus.js` uses the `g*` names.
- The proxy exposes only the signals `g-signal` and `g-properties-changed`.
  `g-name-appeared` and `g-name-lost` do not exist on this GLib, so reconnect uses
  `Gio.bus_watch_name_on_connection`, which also reports the current ownership on the
  first loop iteration. `g_name_has_owner` is not exposed; use `get_name_owner()`.
- A proxy created from construct properties never runs `g_dbus_proxy_setup()` (which is
  not exposed to gjs), so it can make calls but **never receives signals**. Verified
  2026-11-05: `g-signal` delivered nothing on such a proxy and delivered
  `StatusChanged` on the same object built with `new_for_bus_sync`. The bridge therefore
  uses `new_for_bus_sync`, which also keeps working when the name has no owner at
  construction time (verified: proxy built while the daemon was stopped, signals arrived
  after the daemon started).
- `g-signal` delivery was verified live: `SetPaused(true)` produced
  `StatusChanged('paused')` at a subscribed client.
- Live bridge check (`gjs -m tests/testBridgeLive.js`, 2026-11-05): the proxy builds with
  the names above, the watcher fires, settings and the exclusion list are read, and the
  marshalled `SubmitItem` body reaches the daemon and writes a row in 7.4 ms at the client.

### Meta API facts verified against the Meta-18 typelib (2026-11-05)

Checked with `GI_TYPELIB_PATH=/usr/lib64/mutter-18 gjs -m tests/testReadPath.js`, which runs
headless (no shell) and asserts each of these:

- The selection is reached through the display: `global.display.get_selection()`.
  `Meta.Selection.get_selection()` **does not exist** — there is no
  `meta_selection_get_selection` in the typelib.
- `SelectionSource.read_async(mimetype, cancellable, callback)` takes three arguments in gjs
  (`user_data` is not passed); there is no `io_priority` argument.
- `read_finish()` returns a **`Gio.InputStream`**, not `GBytes` (mutter's gir and the runtime
  type both confirm it). The payload is obtained with `read_bytes_async`, which yields
  `GLib.Bytes`. Reading asynchronously keeps the shell main loop free.
- The `a{say}` dictionary value must be the `GLib.Bytes` itself. Wrapping it in an array
  (`[bytes]`) marshals a single byte instead of the payload.
- `SelectionType` values are `SELECTION_PRIMARY`, `SELECTION_CLIPBOARD`, `SELECTION_DND`.
- `add_keybinding(name, settings, flags, handler)` reads the accelerator as a string array
  under a schema key named exactly `name` with type `as`, so the schema key is `open-popup`.

### StatusChanged states

`StatusChanged(s state)` is the invalidation channel for the state the extension caches.
The daemon emits:

| State | Meaning for the extension |
|---|---|
| `paused` / `resumed` | ingest on or off |
| `settings-changed` | cached settings (e.g. `max_item_bytes`) are stale → re-read |
| `exclusions-changed` | cached exclusion list is stale → re-read |

Without the last two the extension would keep filtering with a stale exclusion list, so a
newly excluded app would still get its copies stored. Covered by `daemon/tests/signals.rs`
and asserted live by `tests/testBridgeLive.js`.

---

## Clipboard Observation (Ingest Path)

```js
const selection = global.display.get_selection();
selection.connect('owner-changed', (sel, type, source) => {
  if (type !== Meta.SelectionType.SELECTION_CLIPBOARD) return;
  if (this._paused || this._ignoreNext) { this._ignoreNext = false; return; }

  const sourceApp = this._getFocusedAppId();
  if (this._isAppExcluded(sourceApp)) return;

  const mimes = source.get_mimetypes();
  if (this._isPasswordOrConcealed(mimes)) return;

  const allowedMimes = this._filterAllowedMimes(mimes);
  if (allowedMimes.length === 0) return;

  // Coalesce multi-MIME representations asynchronously (non-blocking).
  // read_finish gives a GInputStream, so the bytes come from read_bytes_async.
  this._readPayloadsAsync(source, allowedMimes, (payloadsDict) => {
    if (Object.keys(payloadsDict).length === 0) return;

    // Fire-and-forget D-Bus submit with GVariant dictionary (zero-copy, NO Array.from).
    // Each value is the GLib.Bytes itself, not [bytes].
    this._daemonProxy.call(
      'SubmitItem',
      new GLib.Variant('(a{say}s)', [payloadsDict, sourceApp]),
      Gio.DBusCallFlags.NONE,
      -1,
      null,
      () => {}
    );
  });
});
```

### Hot Path Performance Rules

- **Zero-copy binary transfer:** Pass `GLib.Bytes` directly into `GLib.Variant.new('ay', bytes)`.
  **Never call `Array.from(bytes)`**, which causes severe SpiderMonkey GC frame hitches.
- **50 ms debounce** per selection event to coalesce multi-event copies from complex applications.
- **Allowed MIME types:** `text/plain`, `text/plain;charset=utf-8`, `UTF8_STRING`, `STRING`, `text/rtf`, `text/html`,
  `image/png`, `image/jpeg`, `image/webp`, `text/x-uri`, `x-special/gnome-copied-files`.
- **Password Manager hints check:**
  - `application/x-kde-passwordManagerHint` (e.g. KeePassXC)
  - `x-kde-passwordManagerHint`
  - `application/x-gnome-concealed`
  - `org.nspasteboard.ConcealedType`
  If any of these MIME types are present in `source.get_mimetypes()`, the copy is skipped immediately.
- Source application ID is sampled from `global.display.get_focus_window()` at copy time.

---

## Global Keybinding

```js
global.display.add_keybinding(
  'open-clipboard-manager',
  this._settings,
  Meta.KeyBindingFlags.NONE,
  () => this._togglePopup()
);
```

- Verified signature on Meta 18 accepts `Gio.Settings` directly.
- Key accelerator is user-configurable in GSettings (defaults to `<Super>V`).
- Cleanly removed with `global.display.remove_keybinding('open-clipboard-manager')` on disable.

---

## Shell-Native Popup UI & Focus

- Built with `St.Widget` / `PopupMenu` inside GNOME Shell.
- **Positioning:**
  - Queries `global.get_pointer()` to open **directly at the mouse cursor position** (Maccy style),
    with screen edge clamping.
  - Option to open centered or directly under the top-panel status indicator.
- **Keyboard Focus & Modal Grab:**
  - Uses `Main.pushModal(this._popup)` to instantly capture all keyboard and pointer input.
  - Completely eliminates Wayland window focus-stealing prevention and activation issues.
- **Dismissal:**
  - Outside click or `Escape` key immediately pops the modal and closes the UI.
- **Paged Rendering:**
  - Displays search entry at the top, focused on open.
  - Queries only visible metadata rows (`GetHistory` / `SearchHistory` limit 15) from the daemon.
  - Renders lightweight Clutter labels and cached thumbnail icons (< 2ms render time).

---

## Automatic Paste Engine

```js
_pasteItem(itemId, asPlainText = false) {
  // 1. Fetch item content bytes from daemon
  this._daemonProxy.call('GetItemContent', new GLib.Variant('(ss)', [itemId, asPlainText ? 'text/plain' : '']), ... , (res) => {
    const [mime, bytes] = this._parseContent(res);

    // 2. Restore clipboard owner
    const source = Meta.SelectionSourceMemory.new(mime, bytes);
    global.display.get_selection().set_owner(Meta.SelectionType.SELECTION_CLIPBOARD, source);

    // 3. Dismiss modal and return focus to previous active window
    this._closePopup();
    if (this._previousWindow) {
      this._previousWindow.focus(global.get_current_time());
    }

    // 4. Synthesize Ctrl+V via Clutter virtual keyboard
    if (this._settings.get_boolean('auto-paste')) {
      const seat = Clutter.get_default_backend().get_default_seat();
      const keyboard = seat.create_virtual_device(Clutter.InputDeviceType.KEYBOARD_DEVICE);
      const now = Clutter.get_current_event_time();

      keyboard.notify_keyval(now, Clutter.KEY_Control_L, Clutter.KeyState.PRESS);
      keyboard.notify_keyval(now, Clutter.KEY_v, Clutter.KeyState.PRESS);
      keyboard.notify_keyval(now, Clutter.KEY_v, Clutter.KeyState.RELEASE);
      keyboard.notify_keyval(now, Clutter.KEY_Control_L, Clutter.KeyState.RELEASE);
    }
  });
}
```

- `Enter` or `1`–`9`: auto-pastes selected item directly into target app.
- `Shift+Enter`: sets clipboard owner only (no keystroke injection).
- `Ctrl+Shift+V`: normalizes payload to `text/plain` before pasting.

---

## Top-Panel Indicator (`indicator.js`)

- Icon added to GNOME Shell panel (Maccy paperclip / clipboard icon).
- Left-click: toggles popup menu.
- Right-click / Menu:
  - Monitoring Status (Active / Paused toggle)
  - Clear Unpinned History
  - Preferences... (opens `gnome-extensions prefs`)
  - Quit / Disable

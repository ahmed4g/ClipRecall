# Environment findings (target machine, verified 2026-11-05)

Everything below was measured on the actual target system, not assumed.

## System

| Item | Value | How it was verified |
|---|---|---|
| Distribution | Fedora Linux 44 Workstation | `/etc/os-release` |
| Kernel | 7.2.15-200.fc44.x86_64 | `uname -r` |
| Session | Wayland, GNOME | `XDG_SESSION_TYPE=wayland`, `XDG_CURRENT_DESKTOP=GNOME`, `WAYLAND_DISPLAY=wayland-0` |
| Xwayland | running (`DISPLAY=:0`) | `loginctl show-session` + env |
| GNOME Shell | 50.6 | `gnome-shell --version` |
| mutter | 50.6, ABI 18 (`/usr/lib64/libmutter-18.so.0`) | `rpm -q mutter` |
| GJS | 1.88.1 | `gjs --version` |
| GTK | 4.22.5 (runtime only) | `rpm -q gtk4` |
| libadwaita | 1.9.4 (runtime only) | `rpm -q libadwaita` |
| glib | 2.88.4 | `pkg-config --modversion glib-2.0` |
| SQLite | 3.51.2, FTS5 works (trigram + unicode61), WAL works | verified via python3 sqlite3 probe |
| Rust | 1.99.1 (cargo 1.99.1) | `rustc --version` |
| wl-clipboard | 2.2.1 | `rpm -q wl-clipboard` |
| Secret Service | `org.freedesktop.secrets` present on session bus (gnome-keyring 50.0, libsecret 0.21.8) | `busctl` |
| flatpak | 1.18.4 | `rpm -q flatpak` |
| ydotool + `/dev/uinput` | present | `which ydotool`, `ls /dev/uinput` |
| Extension testing tool | `/usr/bin/gnome-shell-test-tool`, `/usr/bin/gnome-extensions` | `rpm -ql gnome-shell` |

**Not installed (must be added before building):** `gtk4-devel`, `libadwaita-devel`,
`sqlite-devel`, `gjs-devel`, `meson`, `ninja`. `pkg-config` and `cc` are present.

**Network:** `static.crates.io` returns 200, so cargo can fetch dependencies.
`crates.io` HTML returns 403 to HEAD requests (harmless).

## Wayland clipboard & compositor facts

1. **`wlr-data-control` is not supported by mutter 50.**
   `wl-paste --watch` prints:
   `Watch mode requires a compositor that supports the data-control protocol`
   → an ordinary process cannot observe clipboard changes. A clipboard manager
   cannot be built as a standalone daemon on GNOME.

2. **`ext-copy-device-v1` is supported.**
   `wl-copy <<< "probe-text"` then `wl-paste` returned `probe-text`.
   → clients can read and write the clipboard, but only as a client, not as a manager.

3. **The compositor itself is the only reliable observer.**
   Introspection of `Meta-18.typelib` (via GIRepository and gjs):

   - `Meta.Selection`
     - signal `owner-changed(selection, MetaSelectionType, MetaSelectionSource)`
     - methods: `get_mimetypes()`, `set_owner(type, source)`, `unset_owner(type)`,
       `transfer_async(...)`, `transfer_finish(...)`
   - `Meta.SelectionSource`
     - signals `activated`, `deactivated`
     - methods: `get_mimetypes()`, `is_active()`, `read_async(...)`, `read_finish(...)`
   - `Meta.SelectionSourceMemory.new(mime_type, GLib.Bytes)` — memory-backed source,
     used to make the extension the clipboard owner for paste-back.
   - `Meta.SelectionType`: `SELECTION_PRIMARY`, `SELECTION_CLIPBOARD`, `SELECTION_DND`
   - `Meta.Display`: `add_keybinding(name, settings, flags, handler, user_data)`,
     `remove_keybinding(name)`, `get_selection()`, `get_focus_window()`,
     `get_window_by_id(...)`, `set_input_focus(...)`, `unset_input_focus()`
   - `Meta.Stage`: `get_key_focus()`, `set_key_focus()`, `grab_key_focus()`
   - `Meta.Window`: `focus()`, `has_focus()`, `get_gtk_application_id()`,
     `get_sandboxed_app_id()`, `get_startup_id()`, `get_title()`
   - `Meta.KeyBindingFlags`: `NONE`, `PER_WINDOW`, `BUILTIN`, `IS_REVERSED`,
     `NON_MASKABLE`, `IGNORE_AUTOREPEAT`, `NO_AUTO_GRAB`, `CUSTOM_TRIGGER`, `TRIGGER_RELEASE`

   **Keybinding API verified:**
   `global.display.add_keybinding(name, settings, flags, handler, user_data)` accepts
   the extension's `Gio.Settings` directly (signature confirmed via introspection).

4. **mutter stores clipboard content in memory** (`MetaSelectionSourceMemory`), so content
   survives the source application exiting. Reading via `read_async` is the supported path.

5. **Popup placement & Wayland window constraints:**
   - Mutter does not support `wlr-layer-shell`.
   - Standalone Wayland client windows (like GTK4 windows) cannot position themselves at
     arbitrary screen coordinates (e.g. mouse pointer or status bar icon).
   - In contrast, a GNOME Shell extension popup (`St.Widget` / `PopupMenu.PopupMenu`) runs
     directly inside the compositor process and can query `global.get_pointer()` to open
     **at the mouse cursor position** or under the top-panel indicator, exactly matching Maccy.
   - Grabbing input focus via `Main.pushModal()` is instantaneous and bypasses Wayland focus-stealing
     prevention.

6. **Key injection and automatic paste:**
   - While external Wayland client apps cannot inject keystrokes, the extension runs **inside the compositor**.
   - `Clutter.VirtualInputDevice` (`seat.create_virtual_device(Clutter.InputDeviceType.KEYBOARD_DEVICE)`)
     provides `notify_keyval` and `notify_key`.
   - The extension can set the clipboard owner and immediately emit `Ctrl+V` (or `Shift+Insert` for terminals),
     enabling Maccy-style 1-touch auto-paste into the previously focused window.
   - `/dev/uinput` and `ydotool` are also available as a system-level fallback.

7. **Instant search & FTS5 trigram support:**
   - SQLite 3.51.2 on this machine has `trigram` tokenizer built-in.
   - Trigram search allows substring queries across code, URLs, and paths without requiring
     word boundaries.
   - In-memory scoring in Rust provides Maccy-grade fuzzy search.

8. **`org.gnome.Shell.Eval` is restricted** (returns `false`), so the shell cannot be
   introspected from outside. Extension behaviour must be tested inside the shell.

9. **GNOME Shell 50 removed X11 support.** X11 "compatibility" means:
   XWayland clients share the same clipboard (handling targets like `UTF8_STRING` and `STRING`),
   and the daemon can support non-GNOME/X11 environments as a separate mode if needed.

## Consequences for the design

- The GJS extension is mandatory as the capture layer and the popup/paste controller.
- The extension must stay trivial on the hot path: pass `GLib.Bytes` as `GVariant` without JS array
  conversions (`Array.from` forbidden), no hashing, no decoding.
- The popup is built primarily as a Shell-native menu/popup (`St.Widget` / `PopupMenu`), allowing
  it to open instantly at the mouse cursor or top bar icon with reliable modal focus.
- Automatic paste is supported out of the box using Clutter's virtual keyboard device.
- Preferences are housed natively in `prefs.js` using `Adw.PreferencesWindow` talking to the daemon
  over D-Bus.
- Sensitive content detection pairs application exclusion with password manager MIME hints
  (`x-kde-passwordManagerHint`, `application/x-gnome-concealed`), avoiding false positives on high-entropy text.
- Allowlist includes XWayland targets (`UTF8_STRING`, `STRING`).

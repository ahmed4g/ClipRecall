# TODO — open questions and known unknowns

## Answered & Verified on Fedora 44

- [x] **How does the extension register its GNOME Shell shortcut?**
      The product registers its `Gio.Settings` accelerator through `Main.wm.addKeybinding`.
      The raw `global.display.add_keybinding` signature accepts `Gio.Settings`, but GNOME Shell
      filtered out the spike's direct registration.
- [x] **Popup positioning and focus:**
      A standalone GTK4 window cannot position itself at cursor coordinates on Wayland.
      The primary popup is therefore built as a Shell-native popup (`St.Widget` / `PopupMenu`),
      enabling placement at `global.get_pointer()` or under the top-panel status icon, with
      instant keyboard grab via `Main.pushModal()`.
- [x] **Settings window location:**
      Housed natively in `prefs.js` (`Adw.PreferencesWindow`) launched via GNOME Extensions.
      It communicates directly with the Rust daemon over session D-Bus via `Gio.DBusProxy`.
- [x] **Instant subword/fuzzy search:**
      SQLite 3.51.2 on Fedora 44 supports FTS5 with `tokenize = 'trigram'`, allowing substring
      matching without word boundaries. Paired with in-memory Rust scoring for fuzzy matching.
- [x] **Avoiding false positives on passwords/tokens:**
      In addition to app exclusion, inspect MIME hints emitted by Linux password managers
      (`x-kde-passwordManagerHint`, `application/x-gnome-concealed`). Make entropy-based filtering
      opt-in so developer tokens/hashes are not lost.

## Must be measured in Phase 0 Spike

- [ ] Does `Meta.SelectionSource.read_async` succeed for every allowlisted mime type
      after the source app exits?
- [ ] Actual idle CPU and RSS cost of the extension + daemon over 10 minutes (< 1% CPU, < 40 MB RSS).
- [ ] Is primary selection (`SELECTION_PRIMARY`) worth tracking by default, or opt-in?

## Paste-back experiments and follow-up

- [ ] Check whether `Clutter.VirtualInputDevice.notify_keyval` delivers `Ctrl+V` reliably to client
      applications. The Phase 5 baseline uses clipboard restore followed by manual `Ctrl+V`.
- [ ] Extend paste-back coverage to additional client and format combinations, including Nautilus,
      XWayland clients, and rich text.

## Design decisions settled

- **Extension UUID:** `clipboard-manager@project.local` (matches `extension/metadata.json`;
  the spike used `clipboard-spike@project.local`).
- **Popup Architecture:** Shell-native UI with panel icon indicator and mouse-cursor placement.
- **Multi-MIME Support:** Preserve both `text/plain` and rich representations (`text/html`, `text/rtf`)
  in `item_payloads` table.
- **Max Item Size:** Increased to 16 MB default to prevent silent drops of high-res screenshots.
- **Packaging:** Primary target is RPM + systemd user service + user installation script.
  Flatpak cannot sandbox GNOME Shell extensions.

## Deferred / Post-1.0

- Multi-monitor custom coordinate clamping.
- Network / remote clipboard sync (explicitly out of scope).
- OCR search inside images (out of scope).

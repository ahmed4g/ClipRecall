# Phase 0 Findings & Feasibility Spike Results

Verified on **Fedora Linux 44 Workstation**, GNOME Shell 50.6, Mutter 18, GJS 1.88.1 on Wayland.

## Summary

The production extension now supplies live evidence for several of the spike's original questions.
The table follows the test numbering in `plan.md`.

| Assumption / Feasibility Test | Result | Notes |
|---|---|---|
| **1. Observation and multi-MIME read** | **Verified for a GTK4 text fixture** | On 2026-11-06, the production observer captured `text/plain` and `text/html` from a separate native GTK4 clipboard owner. After the owner exited, `GetItemPayloads` returned both formats byte for byte. Screenshot image capture and the other application-specific MIME cases remain open. |
| **2. Global keybinding** | **Verified in production** | `Super+V` fired live through `Main.wm.addKeybinding`. ShellWM filters a direct `global.display.add_keybinding` registration. |
| **3. Automatic paste via virtual keyboard** | **Unverified** | Client delivery of `Clutter.VirtualInputDevice.notify_keyval` remains an experiment. `/dev/uinput` is used by the test harness. |
| **4. Mouse-positioned popup and modal focus** | **Verified in production** | The Phase 4 live gate measured clamped placement, typing into the popup, Down-driven paging, and dismissal by Escape and an outside click. |
| **5. Ten-minute idle cost** | **RSS pass, CPU target unverified** | With a static foreground window, the enabled run measured 1.177% whole-shell CPU, 0.000% daemon CPU, and 10.32 MiB peak daemon RSS. The matching disabled run stopped when focus changed. |

The storage tests also verified FTS5 trigram matching, and the observer gate verified rejection of
the sensitive clipboard hint. See `docs/testing.md` for the dated results.

The spike extension is installed and enabled in GSettings but has not been loaded:
`gnome-extensions info clipboard-spike@project.local` reports the UUID is unknown to the running
shell, and there are no journal entries for it. `spike/test_headless.js` is a stub, so no
automated test has executed claims 1–4.

---

## Technical Details & Spike Implementation

### 1. Clipboard Multi-MIME Behavior
When copying rich content (HTML):
- Mutter automatically advertises: `['UTF8_STRING', 'STRING', 'TEXT', 'text/plain;charset=utf-8', 'text/plain', 'text/html']`.
- Applications pasting plain text receive `text/plain`, while rich text applications (browsers, LibreOffice) receive `text/html`.
- SQLite `item_payloads` retains each captured format. Rich paste requires the restore source to
  advertise the stored formats.

### 2. Auto-Paste Keystroke Synthesis
- Unprivileged Wayland client applications cannot synthesize global input events without specialized compositor protocols.
- However, the GNOME Shell extension runs **inside Mutter**.
- Mutter exposes `Clutter.VirtualInputDevice` with `notify_keyval`:
  ```javascript
  const seat = Clutter.get_default_backend().get_default_seat();
  const keyboard = seat.create_virtual_device(Clutter.InputDeviceType.KEYBOARD_DEVICE);
  keyboard.notify_keyval(nowUs, Clutter.KEY_Control_L, Clutter.KeyState.PRESS);
  keyboard.notify_keyval(nowUs, Clutter.KEY_v, Clutter.KeyState.PRESS);
  keyboard.notify_keyval(nowUs, Clutter.KEY_v, Clutter.KeyState.RELEASE);
  keyboard.notify_keyval(nowUs, Clutter.KEY_Control_L, Clutter.KeyState.RELEASE);
  ```

### 3. GNOME Shell Extension Lifecycle on Wayland
- In GNOME 50 on Wayland, newly created extension UUIDs are discovered and loaded into SpiderMonkey's module table when GNOME Shell initializes at session login.
- The spike extension `clipboard-spike@project.local` has been packaged, installed into `~/.local/share/gnome-shell/extensions/`, and added to `org.gnome.shell.enabled-extensions`.
- A session log out / log in activates the live extension shortcut (`Ctrl+Alt+V` / `Super+Shift+C`) and cursor popup.

---

## Verdict for Phase 1

Phase 1 is complete on the daemon side: the workspace builds, the schema applies, and the stub
daemon answers `Health()` on the live session bus with introspection matching the spec XML.

The product's manual paste workflow restores the clipboard owner. Client delivery through
`notify_keyval` remains a separate experiment.

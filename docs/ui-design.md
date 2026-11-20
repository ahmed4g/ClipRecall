# UI design

## Popup Window (Shell-Native)

Compact, keyboard-first, Maccy-like. Not a heavy desktop window or dashboard.

```
+------------------------------------------------------+
| [ search ]                       [All|Text|Img|File] |
+------------------------------------------------------+
| PINNED                                                |
|  1  preview text ......................  app  12:04   |
|  2  [thumb] image.png                         11:58   |
+------------------------------------------------------+   <- visible separator
| RECENT                                                |
|  3  preview text ......................  app  11:41   |
|  4  [thumb] screenshot ................ app  11:30    |
|  5  https://github.com/p0deje/Maccy ... app  11:15    |
|  ...                                                  |
+------------------------------------------------------+
| 1-9 select · Enter select · Alt+P pin · Esc          |
+------------------------------------------------------+
```

### Positioning & Behavior

- **Positioning options:**
  - *At mouse cursor* (default, Maccy style) — queries `global.get_pointer()` and positions the popup directly at the mouse pointer with screen-edge clamping.
  - *Under top-bar indicator* — docks directly beneath the GNOME panel icon.
  - *Screen center* — centered on active monitor.
- **Focus & Modal Grab:**
  - On open, the search box receives immediate focus via `Main.pushModal()`.
  - Captures all keystrokes and pointer events.
  - Dismisses immediately on `Escape` or clicking anywhere outside the popup bounds.
- **Visual Structure:**
  - Pinned section pinned at the top with a clear visual divider.
  - Quick-select badge numbers (`1`–`9`) shown on the first 9 visible rows.
  - Rows show: quick index, thumbnail icon (for images), plain preview label (sanitized, max 200 chars), source application icon, and timestamp.
  - Lightweight rendering: only visible rows (< 20 items) are rendered in Clutter; content is never loaded into the popup UI.

---

## Keyboard Model

| Key | Action |
|---|---|
| Typing | Instant search (trigram substring index + in-memory fuzzy scoring) |
| Up / Down | Move row selection |
| `1`–`9` | Quick select, restore the clipboard, and close the popup |
| Enter | Restore the selected item to the clipboard and close the popup |
| Alt+`P` | Pin or unpin the selected item |
| Alt+`D` | Delete the selected unpinned item |
| Alt+`Backspace` | Open confirmation to clear unpinned history |
| `Tab` | Cycle type filter (`All` → `Text` → `Image` → `File`) |
| `Esc` | Close popup |

Paste in the target application after selecting an item. The action buttons provide the same
pin, delete, and clear operations as the Alt shortcuts.

---

## Top-Panel Indicator (`indicator.js`)

The GNOME Shell panel icon displays active or paused monitoring.

- Primary click opens history below the icon.
- Middle click toggles pause or resume.
- Secondary click opens the menu for history, monitoring, and clear-unpinned confirmation.
- Keyboard users can open history from the panel menu.

---

## Preferences Window (`prefs.js` - libadwaita)

Native GNOME settings dialog opened via `gnome-extensions prefs`:

1. **General:**
   - Global shortcut (default `<Super>V`).
   - Popup position: At mouse pointer / Under top-bar icon / Screen center.
   - Auto-paste toggle: Enabled (auto-paste on Enter/1-9) vs Disabled (copy to clipboard only).
   - Audio feedback: Play subtle click sound on copy / paste (optional Maccy feel).
2. **History & Retention:**
   - Maximum history entries (default: 2,000).
   - Retention period (default: 30 days).
   - Move copied items to top on duplicate copy (enabled by default).
3. **Appearance:**
   - Row height: Compact / Regular.
   - Thumbnail preview size (default 200px).
   - Show quick-select shortcut numbers (1-9).
   - Follow system light/dark theme.
4. **Privacy & Exclusion:**
   - Ignore password managers (`x-kde-passwordManagerHint`, `application/x-gnome-concealed`).
   - Excluded applications list (e.g. 1Password, Bitwarden, KeePassXC).
   - Opt-in Shannon entropy heuristic filter.
   - Optional encryption at rest with Secret Service key.
5. **Storage & Data:**
   - Clear history (unpinned / all).
   - Database and thumbnail cache disk usage statistics.
   - Export history (JSON / CSV) & Import history.

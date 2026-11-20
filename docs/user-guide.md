# How to use Clipboard Manager

## Paste something you copied earlier

1. In the app where you want to paste, put the cursor in the destination field.
2. Press **Super+V**. You can also click the Clipboard Manager icon in the top panel.
3. Type part of what you copied to search, or move through the list with **↑** and **↓**.
4. Press **Enter** to restore the highlighted item to the clipboard. The popup closes. Press
   **Ctrl+V** or use your app’s **Paste** command to insert it.

With the search box empty, press a displayed number from **1–9** to restore that item immediately.
With a search in progress, digits are part of the search. Press **Esc** to close the popup.

### Paste directly

Press **Alt+Enter** on the highlighted item to restore it and send **Ctrl+V** to the original
window, provided it still has focus. Direct paste is enabled for GNOME Text Editor, Firefox,
and Google Chrome. To use it with other apps that paste with **Ctrl+V**, open **Preferences →
General** and turn on **Direct paste in all apps**. Apps with their own paste shortcut, such as
some terminals, can use the regular **Enter** workflow.

### Keep formatting

Highlight a saved text item and press **Alt+R**, or click **Copy Rich Text**, to restore its
saved HTML or RTF when available. Then use **Paste** in an app that accepts formatted text.
Regular selection restores the item’s primary format, usually plain text for a browser copy.

## Manage history

| Action | How to do it |
| --- | --- |
| Keep an item at the top | Highlight it and press **Alt+P** or click **Pin**. Use the same shortcut to unpin it. |
| Delete one item | Highlight an unpinned item and press **Alt+D** or click **Delete**. Unpin a pinned item first. |
| Clear unpinned items | Press **Alt+Backspace** in the popup or click **Clear Unpinned**, then confirm. |
| Clear everything, including pins | Go to **Preferences → Storage → Clear all history** and confirm. |
| Pause or resume capture | Right-click the panel icon and choose **Pause Monitoring** or **Resume Monitoring**. Middle-click also toggles capture. |

## Change your preferences

Right-click the panel icon and choose **Preferences…**. You can also open it from a terminal:

```bash
gnome-extensions prefs clipboard-manager@project.local
```

- **General** lets you change the history shortcut, maximum entries, retention days, and direct paste.
  History limits apply when new items are saved. Pinned items are kept.
- **Appearance** lets you use compact rows and change the thumbnail display size.
- **Privacy** lets you exclude an application by its exact application ID or window title.
  **Ignore password manager copies** uses sensitive-content hints from apps that provide them.
  To protect saved copies and previews, unlock GNOME Keyring and turn on **Encrypt history at rest**.
  Encrypted image entries show a generic icon.
- **Storage** lets you clear history or export it. JSON includes clipboard contents. CSV includes
  previews and metadata. Both export formats are readable files, so choose where you save them
  with the contents in mind. [Security and privacy](security.md)

## If something doesn’t work

- **Super+V opens GNOME notifications.** Set a different history shortcut in **Preferences →
  General**, or change the notification shortcut in GNOME Settings.
- **Alt+Enter closes the popup without inserting text.** Use your app’s **Paste** command.
  Check whether the app accepts **Ctrl+V**, whether the original window stayed focused, and
  whether **Direct paste in all apps** is enabled for that app.
- **History shows locked entries.** Unlock GNOME Keyring, then reopen the popup.
- **The popup is missing after an install or update.** Log out and back in. For a new installation,
  enable the extension with `gnome-extensions enable clipboard-manager@project.local` if needed.

# Security and privacy model

## Guarantees

1. **Local only:** Zero network requests, zero analytics, zero cloud sync. The daemon opens no outbound sockets.
2. **Nothing is executed or parsed dangerously:** Clipboard content is never evaluated, never executed, never opened as a URI, never passed through Pango markup or HTML formatters, and never used in shell commands. It is treated as opaque data.
3. **No content logging:** Tracing and logging record only UUIDs, MIME types, byte counts, and execution latencies — never text or image content. Automated tests assert that no clipboard string ever appears in journald logs.
4. **Session D-Bus exposure:** Metadata methods return preview snippets and flags. The popup uses `GetItemContent` or `GetItemPayloads` for paste. Clients with access to the same session bus can call these methods while the history is unlocked.
5. **File permissions:** The daemon uses mode `0600` for the history database and exports, and mode `0700` for the application data and thumbnail directories.

---

## Sensitive Content & Password Managers

### Deterministic Detection (MIME Hints)
Modern Linux and cross-platform password managers flag sensitive clipboard entries using specialized MIME types. With **Ignore password manager copies** enabled by default, the extension discards items with any of the following MIME targets before reading:

- `application/x-kde-passwordManagerHint` (KeePassXC, KDE apps)
- `x-kde-passwordManagerHint`
- `application/x-gnome-concealed` (GNOME Password Safe, libsecret)
- `org.nspasteboard.ConcealedType` (Cross-platform Electron apps)

### Application Exclusion
Users can exclude applications by App ID (e.g. `org.keepassxc.KeePassXC`, `1Password`, `com.bitwarden.desktop`). Any copy initiated while an excluded application has focus is silently dropped.

### Heuristic Entropy Filtering (Opt-In)
- High-entropy text analysis (Shannon entropy) can detect one-time passwords and cryptographic keys.
- **Implemented rule** (`daemon/src/db/storage.rs::looks_like_token`): the payload must be ≥ 24 bytes,
  contain no whitespace, contain both letters and digits, and have byte-level Shannon entropy ≥ 3.5.
  Byte-level entropy alone cannot separate prose from tokens, so the shape checks come first.
- **Why it is opt-in (`filter_entropy = 0` by default):** Aggressive entropy filters frequently cause false positives for developers (Git commit hashes, base64 data, UUIDs, and API tokens get deleted when the user intentionally copied them). Making it an explicit toggle in Privacy settings ensures users retain full control.

---

## Encryption at Rest

- **Preferences → Privacy → Encrypt history at rest** starts off. Enabling it converts existing history and protects future copies. Turning it off converts the saved history back to plaintext.
- The daemon generates a per-database key in the unlocked default Secret Service collection. Clipboard payloads and previews use AES-256-GCM, and a keyed hash supports deduplication. MIME types, source labels, sizes, timestamps, and pins remain readable in SQLite.
- Encrypted preview search decrypts and scores previews in memory. The daemon checks the keyring before protected operations. When it is unavailable, history lists masked items and rejects paste, search, and new saves.
- Enabling encryption removes plaintext thumbnails, rebuilds the active database file, and truncates its WAL. Image entries use a generic icon while encryption is enabled. Backups and copies made before conversion are outside the active database.
- JSON and CSV exports are readable files created on request. JSON includes full clipboard payloads, and CSV includes previews.

---

## Retention & Expiry

- Maximum items (default 2,000) and maximum age (default 30 days) are enforced when new items are saved.
- Pinned items are strictly exempt from automatic eviction.
- Clear unpinned and Clear all delete matching history entries and payloads, then remove cached thumbnails for deleted entries.

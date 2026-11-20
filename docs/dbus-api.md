# D-Bus API specification

Bus name: `org.clipboardmanager.Daemon`
Object path: `/org/clipboardmanager/Daemon`
Interface: `org.clipboardmanager.Daemon`
Scope: session bus (per user). Activated by `org.clipboardmanager.Daemon.service`
(systemd user service, `dbus-1/services` unit).

## Row type

```
Row = (s id, s primary_mime, s preview, t sort_time, b pinned, b has_thumbnail, s source_app)
signature: (ssstbbs)
```

`sort_time` is `last_used_at` when the entry has been used, otherwise `created_at`. It is the value
history is ordered by (`pinned DESC, sort_time DESC, rowid DESC`). Timestamps are unix
milliseconds: with second resolution two copies in the same second shared a `sort_time` and the
order between them fell back to insertion order, so a re-copy could not move an item to the top.
The `rowid DESC` tie-break makes the order deterministic when two rows share a timestamp.

Rows never contain full payload bytes. Content is returned only when requested for paste.

The XML in `data/org.clipboardmanager.Daemon.xml` is the single source of truth for this surface.
`daemon/tests/dbus_spec.rs` fails if the Rust types used by the service drift from it.

## Methods

| Method | In | Out | Notes |
|---|---|---|---|
| `SubmitItem` | `a{say} payloads`, `s source_app` | `s id` | Hot path. Dictionary of `mime -> bytes`. Dedup by primary hash; returns existing id on hit and bumps `last_used_at`. Returns an empty string when the event is skipped (paused, or `IgnoreNext` armed). Content rejected by an ingest filter or exceeding `max_item_bytes` is reported as a `Filtered` / `TooLarge` error. Never blocks caller. |
| `GetHistory` | `i offset`, `i limit`, `s filter` | `a(ssstbbs)` | Paged, ordered by `pinned DESC, sort_time DESC, rowid DESC`. `filter`: `all` (or empty), `text`, `image`, `file`, `pinned`. The type filters are mutually exclusive, so a URI entry is only in `file` and not also in `text`; an unknown filter is an `InvalidArgument` error rather than part of the query. |
| `SearchHistory` | `s query`, `i limit` | `a(ssstbbs)` | Fast FTS5 trigram lookup + in-memory fuzzy scoring. |
| `GetItem` | `s id` | `(ssstbbs)` | Metadata row only. |
| `GetItemContent` | `s id`, `s mime_type` | `(s mime, ay bytes)` | Returns specific format (e.g. `text/plain` or `text/html`). If `mime_type` is empty, returns primary. |
| `GetItemPayloads` | `s id` | `a{say}` | Returns all available formats for multi-flavor paste-back. |
| `GetThumbnail` | `s id` | `ay png` | Pre-generated PNG (max 200px); caller caches in `~/.cache/clipboard-manager/thumbnails/`. |
| `PinItem` / `UnpinItem` | `s id` | `b success` | Pinned items stay at top and survive prune. |
| `DeleteItem` | `s id`, `b force` | `b success` | Removes item and associated payloads. |
| `ClearHistory` | `b include_pinned` | `u deleted` | |
| `SetPaused` | `b paused` | — | Persists the monitoring state and emits `StatusChanged` with `paused` or `resumed`. |
| `IgnoreNext` | — | — | Skip exactly one upcoming clipboard event. |
| `GetSettings` | — | `a{sv}` | Includes the persisted `paused` boolean. |
| `SetSetting` | `s key`, `v value` | — | Change the monitoring state with `SetPaused`. |
| `AddExcludedApp` / `RemoveExcludedApp` | `s app_id` | — | |
| `ListExcludedApps` | — | `as app_ids` | The extension reads this so excluded apps never cross the bus. |
| `ExportHistory` | `s path`, `s format` (`json`, `csv`) | `u count` | |
| `ImportHistory` | `s path`, `s format` | `u count` | |
| `Health` | — | `(s version, u entries, t db_bytes, b busy)` | Diagnostics only; never logs or leaks content. `busy` is true while storage work is in flight on the off-reactor blocking pool; `Health`'s own read runs off the reactor and never makes `busy` trivially true. |

## Signals

| Signal | Payload | Emitted when |
|---|---|---|
| `ItemAdded` | `(ssstbbs)` | New or dedup-updated item (moved to top) |
| `ItemRemoved` | `s id` | Delete or automatic prune |
| `HistoryChanged` | — | Bulk change (clear, import) |
| `StatusChanged` | `s state` | `paused`, `resumed`, `connected`, `disconnected`, `settings-changed`, `exclusions-changed` — the last two invalidate the state the extension caches (settings and the exclusion list), so a change reaches the client that filters before submitting |

## Error model

D-Bus errors namespaced `org.clipboardmanager.Error.*`:

- `Failed` — unexpected internal failure (filesystem, serialization, or bus transport)
- `InvalidArgument` — bad limit, bad filter, invalid format
- `NotFound` — item ID not present
- `TooLarge` — payload above `max_item_bytes` (16 MB)
- `Filtered` — content rejected by password manager hints or app exclusion
- `Busy` — blocking worker pool saturated
- `Encrypted` — database locked and no key is available

`daemon/src/types.rs` `ApiError` (zbus `DBusError` derive, prefix `org.clipboardmanager.Error`) is the
wire representation; `daemon/tests/dbus_spec.rs` asserts the variant names match this list.

## Introspection XML

```xml
<node>
  <interface name="org.clipboardmanager.Daemon">
    <method name="SubmitItem">
      <arg type="a{say}" name="payloads" direction="in"/>
      <arg type="s" name="source_app" direction="in"/>
      <arg type="s" name="id" direction="out"/>
    </method>
    <method name="GetHistory">
      <arg type="i" name="offset" direction="in"/>
      <arg type="i" name="limit" direction="in"/>
      <arg type="s" name="filter" direction="in"/>
      <arg type="a(ssstbbs)" name="rows" direction="out"/>
    </method>
    <method name="SearchHistory">
      <arg type="s" name="query" direction="in"/>
      <arg type="i" name="limit" direction="in"/>
      <arg type="a(ssstbbs)" name="rows" direction="out"/>
    </method>
    <method name="GetItem">
      <arg type="s" name="id" direction="in"/>
      <arg type="(ssstbbs)" name="row" direction="out"/>
    </method>
    <method name="GetItemContent">
      <arg type="s" name="id" direction="in"/>
      <arg type="s" name="mime_type" direction="in"/>
      <arg type="s" name="out_mime" direction="out"/>
      <arg type="ay" name="bytes" direction="out"/>
    </method>
    <method name="GetItemPayloads">
      <arg type="s" name="id" direction="in"/>
      <arg type="a{say}" name="payloads" direction="out"/>
    </method>
    <method name="GetThumbnail">
      <arg type="s" name="id" direction="in"/>
      <arg type="ay" name="png" direction="out"/>
    </method>
    <method name="PinItem">
      <arg type="s" name="id" direction="in"/>
      <arg type="b" name="success" direction="out"/>
    </method>
    <method name="UnpinItem">
      <arg type="s" name="id" direction="in"/>
      <arg type="b" name="success" direction="out"/>
    </method>
    <method name="DeleteItem">
      <arg type="s" name="id" direction="in"/>
      <arg type="b" name="force" direction="in"/>
      <arg type="b" name="success" direction="out"/>
    </method>
    <method name="ClearHistory">
      <arg type="b" name="include_pinned" direction="in"/>
      <arg type="u" name="deleted" direction="out"/>
    </method>
    <method name="SetPaused">
      <arg type="b" name="paused" direction="in"/>
    </method>
    <method name="IgnoreNext"/>
    <method name="GetSettings">
      <arg type="a{sv}" name="settings" direction="out"/>
    </method>
    <method name="SetSetting">
      <arg type="s" name="key" direction="in"/>
      <arg type="v" name="value" direction="in"/>
    </method>
    <method name="AddExcludedApp">
      <arg type="s" name="app_id" direction="in"/>
    </method>
    <method name="RemoveExcludedApp">
      <arg type="s" name="app_id" direction="in"/>
    </method>
    <method name="ListExcludedApps">
      <arg type="as" name="app_ids" direction="out"/>
    </method>
    <method name="ExportHistory">
      <arg type="s" name="path" direction="in"/>
      <arg type="s" name="format" direction="in"/>
      <arg type="u" name="count" direction="out"/>
    </method>
    <method name="ImportHistory">
      <arg type="s" name="path" direction="in"/>
      <arg type="s" name="format" direction="in"/>
      <arg type="u" name="count" direction="out"/>
    </method>
    <method name="Health">
      <arg type="s" name="version" direction="out"/>
      <arg type="u" name="entries" direction="out"/>
      <arg type="t" name="db_bytes" direction="out"/>
      <arg type="b" name="busy" direction="out"/>
    </method>

    <signal name="ItemAdded">
      <arg type="(ssstbbs)" name="row"/>
    </signal>
    <signal name="ItemRemoved">
      <arg type="s" name="id"/>
    </signal>
    <signal name="HistoryChanged"/>
    <signal name="StatusChanged">
      <arg type="s" name="state"/>
    </signal>
  </interface>
</node>
```

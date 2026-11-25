// Session bus bridge to org.clipboardmanager.Daemon. D-Bus activation starts the
// daemon on demand; reconnect logic re-reads the settings and exclusion list when
// the daemon name (re)appears.
//
// Hot path rule: payload values are GLib.Bytes passed straight into the 'ay'
// variant. Never Array.from(bytes) — that copies every byte through SpiderMonkey
// and causes GC frame hitches. `tests/testFilters.js` asserts zero Array.from calls
// on the marshalling path.
//
// Verified on gjs 1.88 / GLib 2.88.4 (this machine):
// - GDBusProxy construct properties are gConnection, gBusType, gName, gObjectPath,
//   gInterfaceName. The older `name`/`objectPath`/`interfaceName` spellings raise
//   "No property name on GDBusProxy", so they are not used.
// - A proxy built from construct properties never runs g_dbus_proxy_setup(), so it
//   makes calls but never receives signals (verified 2026-11-05: g-signal delivered
//   nothing on such a proxy, and setup() is not exposed to gjs). The proxy is therefore
//   built with new_for_bus_sync, which sets it up and delivers signals even when the
//   name has no owner at construction time.
// - Proxy initialization is asynchronous so daemon activation cannot block Shell.
// - The proxy signals available are `g-signal` and `g-properties-changed`.
//   `g-name-appeared` and `g-name-lost` do not exist on this GLib, so name
//   appearance is tracked with Gio.bus_watch_name_on_connection, which also fires
//   immediately when the name is already owned.
// - `g_name_has_owner` is not exposed; use get_name_owner().

import GLib from 'gi://GLib';
import Gio from 'gi://Gio';
import {statusAction} from './filters.js';

export const DAEMON_NAME = 'org.clipboardmanager.Daemon';
export const DAEMON_PATH = '/org/clipboardmanager/Daemon';
export const DAEMON_IFACE = 'org.clipboardmanager.Daemon';

export const DEFAULT_MAX_ITEM_BYTES = 16 * 1024 * 1024;

/// Build the SubmitItem body: (a{say} payloads, s source_app).
export function submitBody(payloads, sourceApp) {
    return new GLib.Variant('(a{say}s)', [payloads, sourceApp]);
}

export class DaemonBridge {
    constructor({onStatusChanged, onConnectionChanged, onSettingsLoaded} = {}) {
        this._onStatusChanged = onStatusChanged || (() => {});
        this._onConnectionChanged = onConnectionChanged || (() => {});
        this._onSettingsLoaded = onSettingsLoaded;

        this.excludedApps = new Set();
        this.maxItemBytes = DEFAULT_MAX_ITEM_BYTES;
        this.filterPasswords = true;
        this.paused = false;
        this.settingsReady = false;
        this._pauseRevision = 0;
        this._settingsRevision = 0;
        this._daemonGeneration = 0;
        this.connected = false;

        this._proxy = null;
        this._watcherId = null;
        this._initializing = new Gio.Cancellable();
        Gio.DBusProxy.new_for_bus(
            Gio.BusType.SESSION, Gio.DBusProxyFlags.NONE, null,
            DAEMON_NAME, DAEMON_PATH, DAEMON_IFACE, this._initializing,
            (_source, result) => {
                let proxy;
                try {
                    proxy = Gio.DBusProxy.new_for_bus_finish(result);
                } catch (err) {
                    if (!this._initializing.is_cancelled())
                        console.error(`[clipboard-manager] proxy initialization failed: ${err.message}`);
                    return;
                }
                if (this._initializing.is_cancelled()) return;
                this._proxy = proxy;
                this._watchDaemon();
            });

        // The watcher reports the current ownership state on the first loop
        // iteration, so `connected` is not trustworthy synchronously; callers that
        // must know before the loop turns should not depend on it.
    }

    _watchDaemon() {
        this._signalId = this._proxy.connect('g-signal', (proxy, sender, signalName, params) => {
            if (signalName === 'StatusChanged') this._handleStatusChanged(params.deepUnpack()[0]);
        });

        this._watcherId = Gio.bus_watch_name_on_connection(
            Gio.DBus.session,
            DAEMON_NAME,
            Gio.BusNameWatcherFlags.NONE,
            (connection, name, nameOwner) => {
                this.connected = true;
                this._onConnectionChanged('appeared');
                this.refresh();
            },
            (connection, name) => {
                this.connected = false;
                this.settingsReady = false;
                this.paused = false;
                this.excludedApps.clear();
                this.maxItemBytes = DEFAULT_MAX_ITEM_BYTES;
                this.filterPasswords = true;
                this._daemonGeneration++;
                this._onConnectionChanged('lost');
            }
        );

    }

    _handleStatusChanged(state) {
        const action = statusAction(state);
        if (action === 'pause' || action === 'resume') {
            this._pauseRevision++;
            this.paused = action === 'pause';
        } else if (action === 'refresh') {
            this.refresh();
        }
        this._onStatusChanged(state);
    }

    /// Re-read the settings and the exclusion list. The daemon is the source of
    /// truth for both; the extension caches them so excluded apps and hinted
    /// copies are dropped before anything crosses the bus.
    refresh() {
        const settingsRevision = ++this._settingsRevision;
        // Load the exclusions before marking settings ready for clipboard capture.
        this._call('ListExcludedApps', null, (reply) => {
            if (settingsRevision !== this._settingsRevision) return;
            this.excludedApps = new Set(reply.deepUnpack()[0]);
            const pauseRevision = this._pauseRevision;
            this._call('GetSettings', null, (settingsReply) => {
                if (settingsRevision !== this._settingsRevision) return;
                const settings = settingsReply.recursiveUnpack()[0];
                const limit = settings.max_item_bytes;
                if (limit !== undefined) this.maxItemBytes = Number(limit);
                this.filterPasswords = settings.filter_passwords !== false;

                const becameReady = !this.settingsReady;
                this.settingsReady = true;
                // A newer status signal can arrive while this request is in flight.
                const paused = settings.paused;
                if (pauseRevision === this._pauseRevision && typeof paused === 'boolean' && paused !== this.paused) {
                    this._handleStatusChanged(paused ? 'paused' : 'resumed');
                } else if (becameReady) {
                    this._onStatusChanged('settings-ready');
                }
                this._onSettingsLoaded?.(settings, this.excludedApps);
            });
        });
    }

    /// Fire-and-forget ingest: the hot path never waits for the reply. The optional
    /// callback is for callers that want to know the outcome (the live check in
    /// tests/testBridgeLive.js); the observer does not use it.
    submit(payloads, sourceApp, onReply) {
        this._call('SubmitItem', submitBody(payloads, sourceApp), (reply) => {
            const id = reply.deepUnpack()[0];
            if (id === '') {
                // Skipped by the daemon (paused or IgnoreNext armed).
            }
            if (onReply) onReply(id);
        });
    }

    /// Read path for the popup. Rows are the canonical (ssstbbs) tuple; the caller
    /// maps them with popupLogic.rowFromTuple. These are not the hot path, so waiting
    /// for the reply is fine, but the call is still async so the shell loop is never
    /// blocked by storage work.
    getHistory(offset, limit, filter, onRows, onError) {
        this._call('GetHistory', new GLib.Variant('(iis)', [offset, limit, filter]), (reply) => {
            if (onRows) onRows(reply.deepUnpack()[0]);
        }, onError);
    }

    search(query, limit, onRows, onError) {
        // The canonical order is query then limit, per data/org.clipboardmanager.Daemon.xml.
        // This was (is) until 2026-11-06, which the daemon rejected with
        // "Expected type string for argument 'string' but got type number" — the first live
        // search the popup made failed here, so the signatures are now pinned by
        // tests/testDbusSignatures.js against the XML.
        this._call('SearchHistory', new GLib.Variant('(si)', [query, limit]), (reply) => {
            if (onRows) onRows(reply.deepUnpack()[0]);
        }, onError);
    }

    searchFiltered(query, limit, filter, onRows, onError) {
        if (filter === 'all') {
            this.search(query, limit, onRows, onError);
            return;
        }
        this._call('SearchFilteredHistory', new GLib.Variant('(sis)', [query, limit, filter]),
            reply => onRows(reply.deepUnpack()[0]), onError);
    }

    /// Fetch one payload for paste-back. Empty mime means the primary.
    getContent(id, mime, onContent, onError) {
        this._call('GetItemContent', new GLib.Variant('(ss)', [id, mime]), (reply) => {
            if (onContent) onContent(reply.deepUnpack());
        }, onError);
    }

    getRichText(id, onContent, onError) {
        this._call('GetItemPayloads', new GLib.Variant('(s)', [id]), (reply) => {
            const payloads = reply.get_child_value(0);
            for (const mime of ['text/html', 'text/rtf']) {
                const payload = payloads.lookup_value(mime, null);
                if (payload?.n_children() > 0) {
                    onContent([mime, payload.deepUnpack()]);
                    return;
                }
            }
            onContent(null);
        }, onError);
    }

    getThumbnail(id, onThumbnail) {
        this._call('GetThumbnail', new GLib.Variant('(s)', [id]), (reply) => {
            onThumbnail(reply.get_child_value(0).get_data_as_bytes());
        });
    }

    pinItem(id, onSuccess) {
        this._call('PinItem', new GLib.Variant('(s)', [id]), (reply) => {
            onSuccess?.(reply.deepUnpack()[0]);
        }, () => onSuccess?.(false));
    }

    unpinItem(id, onSuccess) {
        this._call('UnpinItem', new GLib.Variant('(s)', [id]), (reply) => {
            onSuccess?.(reply.deepUnpack()[0]);
        }, () => onSuccess?.(false));
    }

    deleteItem(id, force, onSuccess) {
        this._call('DeleteItem', new GLib.Variant('(sb)', [id, force]), (reply) => {
            onSuccess?.(reply.deepUnpack()[0]);
        }, () => onSuccess?.(false));
    }

    clearHistory(includePinned, onDeleted) {
        this._call('ClearHistory', new GLib.Variant('(b)', [includePinned]), (reply) => {
            onDeleted?.(reply.deepUnpack()[0]);
        }, () => onDeleted?.(null));
    }

    setPaused(paused, onDone) {
        // StatusChanged remains the source of truth for this.paused.
        this._call('SetPaused', new GLib.Variant('(b)', [paused]), () => onDone?.(true), () => onDone?.(false));
    }

    setSetting(key, value, onDone) {
        // Converting the full history can take longer than the default bus timeout.
        const timeout = key === 'encrypt_at_rest' ? 600_000 : -1;
        this._call('SetSetting', new GLib.Variant('(sv)', [key, value]),
            () => onDone?.(true), () => onDone?.(false), timeout);
    }

    addExcludedApp(id, onDone) {
        this._call('AddExcludedApp', new GLib.Variant('(s)', [id]), () => onDone?.(true), () => onDone?.(false));
    }

    removeExcludedApp(id, onDone) {
        this._call('RemoveExcludedApp', new GLib.Variant('(s)', [id]), () => onDone?.(true), () => onDone?.(false));
    }

    exportHistory(path, format, onExported) {
        this._call('ExportHistory', new GLib.Variant('(ss)', [path, format]), reply => {
            onExported?.(reply.deepUnpack()[0]);
        }, () => onExported?.(null));
    }

    importHistory(path, format, onImported) {
        this._call('ImportHistory', new GLib.Variant('(ss)', [path, format]), reply => {
            onImported?.(reply.deepUnpack()[0]);
        }, () => onImported?.(null), 600_000);
    }

    _call(method, body, onReply, onError, timeout = -1) {
        if (!this._proxy) {
            onError?.();
            return;
        }
        const generation = this._daemonGeneration;
        this._proxy.call(method, body, Gio.DBusCallFlags.NONE, timeout, null, (proxy, result) => {
            let reply;
            try {
                reply = proxy.call_finish(result);
            } catch (err) {
                if (proxy === this._proxy) console.error(`[clipboard-manager] ${method} failed: ${err.message}`);
            }
            if (proxy !== this._proxy) return;
            try {
                if (reply && generation === this._daemonGeneration) onReply?.(reply);
                else onError?.();
            } catch (err) {
                console.error(`[clipboard-manager] ${method} callback failed: ${err.message}`);
            }
        });
    }

    destroy() {
        this.settingsReady = false;
        this._initializing?.cancel();
        if (this._signalId) this._proxy.disconnect(this._signalId);
        if (this._watcherId !== null) Gio.bus_unwatch_name(this._watcherId);
        this._watcherId = null;
        this._proxy = null;
    }
}

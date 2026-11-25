import Clutter from 'gi://Clutter';
import Meta from 'gi://Meta';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import Shell from 'gi://Shell';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';
import {DaemonBridge} from './dbus.js';
import {Debouncer, allowedMimes, isSensitive, pasteMime} from './filters.js';
import {Indicator} from './indicator.js';
import {Popup} from './popup.js';

const DEBOUNCE_MS = 50;
const CHROME_APP_IDS = ['google-chrome.desktop', 'com.google.Chrome.desktop'];
const DIRECT_PASTE_APP_IDS = ['org.gnome.TextEditor.desktop', 'firefox.desktop',
    'org.mozilla.firefox.desktop', ...CHROME_APP_IDS];
const PASTE_MODIFIERS = Clutter.ModifierType.MOD1_MASK | Clutter.ModifierType.CONTROL_MASK |
    Clutter.ModifierType.SHIFT_MASK | Clutter.ModifierType.MOD4_MASK | Clutter.ModifierType.SUPER_MASK |
    Clutter.ModifierType.HYPER_MASK | Clutter.ModifierType.META_MASK;

// Capture filters sensitive and excluded copies, debounces changes, and transfers
// bounded payloads to the daemon. Clipboard contents are never logged.
export default class ClipboardManagerExtension extends Extension {
    enable() {
        this._pasteTimeoutId = 0;
        this._settings = this.getSettings();
        this._capture = new Gio.Cancellable();
        this._clipboardRevision = 0;
        this._bridge = new DaemonBridge({
            onStatusChanged: (state) => {
                if (state === 'paused') this._cancelCapture();
                this._syncIndicator();
            },
            onConnectionChanged: (state) => {
                if (state === 'lost') {
                    this._cancelCapture();
                    this._pauseRequest = null;
                }
                this._syncIndicator();
            },
        });
        this._ownSource = null;
        this._pauseRequest = null;

        this._debouncer = new Debouncer(
            DEBOUNCE_MS,
            (event) => this._flush(event),
            {
                arm: (fn, delayMs) => GLib.timeout_add(GLib.PRIORITY_DEFAULT, delayMs, () => {
                    fn();
                    return GLib.SOURCE_REMOVE;
                }),
                cancel: (id) => GLib.source_remove(id),
            }
        );

        this._popup = new Popup(this._bridge, {
            settings: this._settings,
            onSelect: (row, richText, pasteTarget) => this._restoreOwner(row, richText, pasteTarget),
            // Timings and the position are logged so the live acceptance gate can read them
            // from the journal: popup open at the cursor, query to render, close, and which
            // row a key selected. The log contains metadata only.
            onTiming: (kind, ms, info) => {
                if (kind === 'open') {
                    const anchor = info.anchor ? ` anchor=(${info.anchor[0]}, ${info.anchor[1]})` : '';
                    console.log(`[clipboard-manager] popup open ${ms.toFixed(1)} ms at (${info.x}, ${info.y}) ` +
                        `pointer=(${info.pointer[0]}, ${info.pointer[1]}) ` +
                        `work=(${info.work[0]}, ${info.work[1]}, ${info.work[2]}, ${info.work[3]})${anchor}`);
                } else if (kind === 'render') {
                    console.log(`[clipboard-manager] ${info.rows} rows rendered in ${ms.toFixed(1)} ms ` +
                        `window=${info.start} requests=${info.requests} kind=${info.query === '' ? 'history' : 'search'}`);
                } else if (kind === 'close') {
                    console.log(`[clipboard-manager] popup closed after ${ms.toFixed(0)} ms`);
                } else if (kind === 'select') {
                    console.log(`[clipboard-manager] selected row ${info.id} (index ${info.index})`);
                }
            },
        });

        this._indicator = new Indicator({
            onOpen: (anchor) => this._popup.open(anchor),
            onPause: (paused) => this._setPaused(paused),
            onClear: (anchor) => this._popup.requestClear(anchor),
            onPreferences: () => this.openPreferences(),
        });
        Main.panel.addToStatusArea(this.uuid, this._indicator);
        this._syncIndicator();

        // Registering on global.display alone is not enough on GNOME Shell 50. ShellWM's
        // 'filter-keybinding' handler answers `!(this._allowedKeybindings[name] &
        // Main.actionMode)`, and only Main.wm.addKeybinding() puts a name in that allow list,
        // so a binding added straight to the display is filtered out and never fires.
        // Measured on 2026-11-06: an injected Super+V fired a keybinding registered through
        // Main.wm (org.gnome.shell.keybindings toggle-overview) but not one registered on
        // global.display by this extension or by the Phase 0 spike.
        this._keybindingAction = Main.wm.addKeybinding(
            'open-popup',
            this._settings,
            Meta.KeyBindingFlags.NONE,
            Shell.ActionMode.ALL,
            () => this._popup.open()
        );

        const selection = global.display.get_selection();
        this._ownerChangedId = selection.connect('owner-changed', (sel, type, source) => {
            if (type === Meta.SelectionType.SELECTION_CLIPBOARD) this._onOwnerChanged(sel, source);
        });
    }

    disable() {
        if (this._pasteTimeoutId) GLib.source_remove(this._pasteTimeoutId);
        this._pasteTimeoutId = 0;
        if (this._ownerChangedId) {
            global.display.get_selection().disconnect(this._ownerChangedId);
            this._ownerChangedId = null;
        }
        this._indicator?.destroy();
        this._indicator = null;
        if (this._popup) this._popup.destroy();
        this._popup = null;
        this._capture.cancel();
        this._pasteKeyboard = null;
        this._ownSource = null;
        if (this._debouncer) this._debouncer.cancel();
        if (this._keybindingAction !== null) {
            // The allow-list entry is removed through Main.wm, which is what put it there.
            Main.wm.removeKeybinding('open-popup');
            this._keybindingAction = null;
        }
        if (this._bridge) this._bridge.destroy();
        this._bridge = null;
        this._settings = null;
    }

    _syncIndicator() {
        if (!this._indicator || !this._bridge) return;
        this._indicator.setPaused(this._bridge.paused);
        this._indicator.setAvailable(this._bridge.settingsReady);
    }

    _setPaused(paused) {
        const bridge = this._bridge;
        if (!bridge?.settingsReady || this._pauseRequest) return;
        const request = {};
        this._pauseRequest = request;
        if (paused) this._cancelCapture();
        bridge.setPaused(paused, (ok) => {
            if (this._bridge !== bridge || this._pauseRequest !== request) return;
            this._pauseRequest = null;
            if (!ok) Main.notifyError('Clipboard Manager', 'Could not update monitoring.');
        });
    }

    _restoreOwner(row, richText, pasteTarget) {
        const originalRevision = this._clipboardRevision;
        const bridge = this._bridge;
        const request = {};
        this._restoreRequest = request;
        const isCurrent = () => this._bridge === bridge && this._restoreRequest === request;
        const restore = content => {
            if (!isCurrent() || (pasteTarget && this._clipboardRevision !== originalRevision)) return;
            if (!content) {
                Main.notifyError('Clipboard Manager', 'No rich text is available for this item.');
                return;
            }
            const [mime, bytes] = content;
            if (!bytes || bytes.length === 0) return;

            const selection = global.display.get_selection();
            const app = pasteTarget ? Shell.WindowTracker.get_default().get_window_app(pasteTarget)?.get_id() : null;
            // Chrome's tested Wayland paste path accepts the unqualified text format.
            let offer = pasteMime(mime, bytes);
            if (CHROME_APP_IDS.includes(app) && offer === 'text/plain;charset=utf-8')
                offer = 'text/plain';
            const source = Meta.SelectionSourceMemory.new(offer, GLib.Bytes.new(bytes));
            // Setting the owner fires owner-changed, which is our own write and must not
            // be recorded as a copy.
            this._ownSource = source;
            try {
                selection.set_owner(Meta.SelectionType.SELECTION_CLIPBOARD, source);
                if (pasteTarget) this._pasteInto(pasteTarget, app, isCurrent);
            } catch (err) {
                this._ownSource = null;
                throw err;
            }
        };
        if (richText) {
            bridge.getRichText(row.id, restore, () => {
                if (isCurrent()) Main.notifyError('Clipboard Manager', 'Could not read rich text from history.');
            });
        } else {
            bridge.getContent(row.id, row.primaryMime, restore, () => {
                if (isCurrent()) Main.notifyError('Clipboard Manager', 'Could not restore this item from history.');
            });
        }
    }

    _pasteInto(target, app, isCurrent) {
        if (!this._settings.get_boolean('direct-paste-all-apps') && !DIRECT_PASTE_APP_IDS.includes(app)) {
            Main.notify('Clipboard Manager', 'Copied to clipboard. Use Paste in this app.');
            return;
        }
        if (this._pasteTimeoutId) GLib.source_remove(this._pasteTimeoutId);
        this._pasteTimeoutId = 0;
        const revision = this._clipboardRevision;
        const deadline = GLib.get_monotonic_time() + 500_000;
        this._pasteTimeoutId = GLib.timeout_add(GLib.PRIORITY_DEFAULT, 15, () => {
            if (!isCurrent() || this._clipboardRevision !== revision || this._popup?.isOpen ||
                global.display.get_focus_window() !== target || Main.actionMode !== Shell.ActionMode.NORMAL ||
                GLib.get_monotonic_time() >= deadline) {
                this._pasteTimeoutId = 0;
                return GLib.SOURCE_REMOVE;
            }
            if (global.get_pointer()[2] & PASTE_MODIFIERS) return GLib.SOURCE_CONTINUE;

            this._pasteTimeoutId = 0;
            const seat = global.stage.context.get_backend().get_default_seat();
            // Mutter dispatches these events on its input thread. Retain the
            // virtual device instead of disposing it while events are queued.
            const keyboard = this._pasteKeyboard ??=
                seat.create_virtual_device(Clutter.InputDeviceType.KEYBOARD_DEVICE);
            const time = GLib.get_monotonic_time();
            keyboard.notify_keyval(time, Clutter.KEY_Control_L, Clutter.KeyState.PRESSED);
            try {
                keyboard.notify_keyval(time, Clutter.KEY_v, Clutter.KeyState.PRESSED);
                keyboard.notify_keyval(time, Clutter.KEY_v, Clutter.KeyState.RELEASED);
            } finally {
                keyboard.notify_keyval(time, Clutter.KEY_Control_L, Clutter.KeyState.RELEASED);
            }
            return GLib.SOURCE_REMOVE;
        });
    }

    _onOwnerChanged(selection, source) {
        this._clipboardRevision++;
        // A replaced or disconnected owner invalidates every outstanding MIME read.
        this._cancelCapture();
        if (!source || !this._bridge) return;
        if (source === this._ownSource) return;
        this._ownSource = null;
        if (!this._canCapture(this._bridge)) return;

        const mimes = source.get_mimetypes();
        const sensitive = isSensitive(mimes);
        if (this._bridge.filterPasswords && sensitive) return;

        const sourceApp = this._focusedAppId();
        if (this._bridge.excludedApps.has(sourceApp)) return;

        const allowed = allowedMimes(mimes);
        if (allowed.length === 0) return;

        // Coalesce multi-event copies from complex applications.
        this._debouncer.schedule({selection, allowed, sourceApp, sensitive});
    }

    _canCapture(bridge) {
        return this._bridge === bridge && bridge?.settingsReady && !bridge.paused && !this._pauseRequest;
    }

    _cancelCapture() {
        this._capture.cancel();
        this._capture = new Gio.Cancellable();
        this._debouncer?.cancel();
    }

    _flush({selection, allowed, sourceApp, sensitive}) {
        const bridge = this._bridge;
        if (!this._canCapture(bridge)) return;
        const capture = this._capture;
        const isCurrent = () => !capture.is_cancelled() && this._canCapture(bridge);
        const limit = bridge.maxItemBytes;
        const payloads = {};
        // Let the daemon enforce its current policy even if the extension's setting is stale.
        if (sensitive) payloads['application/x-gnome-concealed'] = GLib.Bytes.new(new Uint8Array());
        let total = 0;

        const readNext = index => {
            if (!isCurrent()) return;
            if (index === allowed.length) {
                if (total > 0) bridge.submit(payloads, sourceApp);
                return;
            }
            const mime = allowed[index];
            const output = Gio.MemoryOutputStream.new_resizable();
            // Ask Mutter for the current owner, rather than retaining the Wayland
            // source from the signal across asynchronous reads.
            selection.transfer_async(Meta.SelectionType.SELECTION_CLIPBOARD, mime,
                limit - total + 1, output, capture, (manager, result) => {
                    try {
                        manager.transfer_finish(result);
                        if (!isCurrent()) return;
                        output.close(null);
                        const bytes = output.steal_as_bytes();
                        total += bytes.get_size();
                        if (total > limit) {
                            console.log(`[clipboard-manager] skipped: copy exceeds the ${limit} byte limit`);
                            return;
                        }
                        if (bytes.get_size() > 0) payloads[mime] = bytes;
                    } catch (err) {
                        if (!isCurrent()) return;
                        console.error(`[clipboard-manager] selection transfer failed for ${mime}: ${err.message}`);
                    }
                    // Let Mutter finish unwinding the current transfer callback.
                    GLib.idle_add_once(GLib.PRIORITY_DEFAULT_IDLE, () => readNext(index + 1));
                });
        };
        readNext(0);
    }

    _focusedAppId() {
        const window = global.display.get_focus_window();
        if (!window) return 'unknown';
        return window.get_gtk_application_id() || window.get_title() || 'unknown';
    }
}

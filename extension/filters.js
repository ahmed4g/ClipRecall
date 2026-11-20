// Pure clipboard MIME and ingest filter logic, shared by the extension and the headless tests
// (`gjs -m tests/testFilters.js`). No gi:// imports so it runs without a shell.

export const ALLOWED_MIMES = new Set([
    'text/plain',
    'text/plain;charset=utf-8',
    'UTF8_STRING',
    'STRING',
    'text/rtf',
    'text/html',
    'image/png',
    'image/jpeg',
    'image/webp',
    'text/uri-list',
    'text/x-uri',
    'x-special/gnome-copied-files',
]);

// Password manager hints are the only reliable signal the protocol carries.
export const SENSITIVE_HINTS = new Set([
    'application/x-kde-passwordManagerHint',
    'x-kde-passwordManagerHint',
    'application/x-gnome-concealed',
    'org.nspasteboard.ConcealedType',
]);

export function isSensitive(mimes) {
    return mimes.some((m) => SENSITIVE_HINTS.has(m));
}

export function allowedMimes(mimes) {
    return mimes.filter((m) => ALLOWED_MIMES.has(m));
}

export function pasteMime(mime, bytes) {
    if (mime !== 'text/plain') return mime;

    // Firefox's GTK clipboard reader needs an explicit UTF-8 offer. Validate the
    // encoding here so the original bytes can be offered without conversion.
    try {
        new TextDecoder('utf-8', {fatal: true}).decode(bytes);
        return 'text/plain;charset=utf-8';
    } catch (_) {
        return mime;
    }
}

export function exceedsSize(totalBytes, maxBytes) {
    return totalBytes > maxBytes;
}

// StatusChanged states and what the bridge does with them. The daemon is the source of
// truth for the pause state, the settings and the exclusion list; the bridge caches the
// last two so filtered copies never cross the bus, so the cached copies are dropped when
// the daemon announces a change.
export function statusAction(state) {
    if (state === 'paused') return 'pause';
    if (state === 'resumed') return 'resume';
    if (state === 'settings-changed' || state === 'exclusions-changed') return 'refresh';
    return 'ignore';
}

// Debouncer: coalesce selection events that arrive within delayMs, so copies
// from applications that set the selection several times produce one entry.
// The scheduler is injected so tests can drive it without a main loop.
export class Debouncer {
    constructor(delayMs, onFire, scheduler) {
        this._delayMs = delayMs;
        this._onFire = onFire;
        this._scheduler = scheduler;
        this._pending = null;
        this._timer = null;
    }

    schedule(value) {
        this._pending = value;
        if (this._timer !== null) {
            this._scheduler.cancel(this._timer);
            this._timer = null;
        }
        this._timer = this._scheduler.arm(() => {
            this._timer = null;
            const value = this._pending;
            this._pending = null;
            this._onFire(value);
        }, this._delayMs);
    }

    cancel() {
        if (this._timer !== null) {
            this._scheduler.cancel(this._timer);
            this._timer = null;
        }
        this._pending = null;
    }
}

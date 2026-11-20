// Pure popup logic, shared by the shell UI (popup.js) and the headless tests
// (`gjs -m tests/testPopupLogic.js`). No gi:// imports so it runs without a shell.
//
// Everything the Phase 4 acceptance gate asserts that can be checked without a live
// session lives here: the pointer position clamped to the monitor work area, the quick
// select mapping, the pinned/recent sections, keyboard navigation, and the paged render
// window. The shell UI in popup.js is thin on purpose so these rules stay testable.

// Paged rendering: at most this many rows are queried from the daemon on demand.
export const MAX_VISIBLE = 20;

// Quick select badges 1-9 on the first nine visible rows.
export const QUICK_SELECT_MAX = 9;

// Keystrokes inside this window produce one query, so typing a word does not send
// one D-Bus call per character.
export const SEARCH_DEBOUNCE_MS = 20;

// The canonical history row is (ssstbbs): id, primary_mime, preview, sort_time,
// pinned, has_thumbnail, source_app. deepUnpack() gives the tuple as an array; this
// turns it into the shape the UI uses. sort_time is unix milliseconds.
export function rowFromTuple(tuple) {
    return {
        id: tuple[0],
        primaryMime: tuple[1],
        preview: tuple[2],
        sortTime: Number(tuple[3]),
        pinned: Boolean(tuple[4]),
        hasThumbnail: Boolean(tuple[5]),
        sourceApp: tuple[6] || '',
    };
}

// Row badge type. These are the same buckets as the daemon's GetHistory filters, so
// a row shows up in exactly one of them.
export function rowType(row) {
    if (row.primaryMime.startsWith('image/')) return 'image';
    if (['text/uri-list', 'text/x-uri', 'x-special/gnome-copied-files'].includes(row.primaryMime)) return 'file';
    return 'text';
}

// One line per row: newlines in a preview would break the row layout.
export function displayPreview(row, maxChars = 120) {
    const oneLine = row.preview.replace(/\s+/g, ' ').trim();
    return oneLine.length > maxChars ? `${oneLine.slice(0, maxChars - 1)}\u2026` : oneLine;
}

export function quickSelectIndex(index) {
    return index < QUICK_SELECT_MAX ? index + 1 : null;
}

// A digit is quick select only while the search box is empty. Once the user has typed a
// query, the digit is part of that query; otherwise a search for "1999" would select a row
// instead of searching. The gate therefore presses the digit on a freshly opened popup.
export function digitIsQuickSelect(entryText) {
    return entryText === '';
}

// Which change is a quick select key press. Clutter.Text consumes printable keys before the
// popup's key handler runs, so a digit pressed while the entry is empty arrives as text, not
// as a key event (measured live on 2026-11-06: pressing 1 produced a search for "1" instead of
// selecting a row). The rule is therefore applied to the change itself: an empty entry that
// becomes a single digit is the selection key.
export function quickSelectFromChange(previousText, newText) {
    if (!digitIsQuickSelect(previousText)) return null;
    if (newText.length !== 1) return null;
    const digit = Number(newText);
    if (!Number.isInteger(digit) || digit < 1 || digit > QUICK_SELECT_MAX) return null;
    return digit;
}

// Pinned section then recent section. The separator is only drawn when both have rows.
export function groupSections(rows) {
    const pinned = rows.filter((r) => r.pinned);
    const recent = rows.filter((r) => !r.pinned);
    return [
        {name: 'pinned', rows: pinned, separator: false},
        {name: 'recent', rows: recent, separator: pinned.length > 0 && recent.length > 0},
    ];
}

// The popup opens at the pointer, but a pointer near the right or bottom edge would put
// part of it off screen, so the position is clamped to the monitor work area.
export function clampToWorkArea(x, y, width, height, workArea) {
    const maxX = Math.max(workArea.x, workArea.x + workArea.width - width);
    const maxY = Math.max(workArea.y, workArea.y + workArea.height - height);
    return {
        x: Math.min(Math.max(x, workArea.x), maxX),
        y: Math.min(Math.max(y, workArea.y), maxY),
    };
}

// Keyboard navigation over the visible rows: Up/Down move, Home/End jump, and the
// selection never wraps past the end of the list.
export class Navigator {
    constructor(count = 0) {
        this._count = count;
        this._index = count > 0 ? 0 : -1;
    }

    setCount(count) {
        this._count = count;
        if (this._index >= count) this._index = count > 0 ? count - 1 : -1;
        else if (count > 0 && this._index < 0) this._index = 0;
    }

    get index() {
        return this._index;
    }

    move(delta) {
        if (this._count === 0) return -1;
        this._index = Math.min(Math.max(this._index + delta, 0), this._count - 1);
        return this._index;
    }

    setIndex(index) {
        if (this._count === 0) return -1;
        this._index = Math.min(Math.max(index, 0), this._count - 1);
        return this._index;
    }

    home() {
        if (this._count === 0) return -1;
        this._index = 0;
        return this._index;
    }

    end() {
        if (this._count === 0) return -1;
        this._index = this._count - 1;
        return this._index;
    }
}

// Paged render window: the UI renders at most maxVisible rows at a time and asks for the
// next page only when navigation reaches past what is already loaded.
//
// The window slides with the selection. With a fixed window of the first maxVisible loaded
// rows, navigation could never reach row maxVisible, so the second page would never be
// fetched and the paging rule would never run live.
export class PageWindow {
    constructor(fetchPage, maxVisible = MAX_VISIBLE) {
        this._fetch = fetchPage;
        this.maxVisible = maxVisible;
        this.rows = [];
        this.exhausted = false;
        this.requests = 0;
        this.start = 0;
    }

    // index is the absolute position in the loaded rows. Returns true when a fetch was
    // needed; a request inside the loaded rows must not cross the bus.
    ensure(index) {
        if (index < 0 || index < this.rows.length || this.exhausted) return false;
        this.requests++;
        this._fetch({offset: this.rows.length, limit: this.maxVisible});
        return true;
    }

    append(rows) {
        if (rows.length === 0) {
            this.exhausted = true;
            return;
        }
        this.rows.push(...rows);
    }

    // The window that contains the selection, at most maxVisible rows.
    windowFor(index) {
        const last = Math.min(Math.max(index, 0), Math.max(this.rows.length - 1, 0));
        this.start = Math.floor(last / this.maxVisible) * this.maxVisible;
        return this.rows.slice(this.start, this.start + this.maxVisible);
    }

    get count() {
        return this.rows.length;
    }
}

// How many rows navigation can reach: the loaded rows, plus the row that is not loaded yet
// when more pages may exist. Without that row the selection clamps at the last loaded row and
// the next page is never requested, so the paging rule cannot run live — measured on
// 2026-11-06, 20 Down presses on a full 20-row window fetched nothing. The row is reachable
// only for the moment between the key press and the reply, which is exactly when the fetch
// happens.
export function reachableCount(pager) {
    return pager.count + (pager.exhausted ? 0 : 1);
}

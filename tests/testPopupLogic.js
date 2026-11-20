// Headless unit tests for the popup logic. Run with:
//   gjs -m tests/testPopupLogic.js
//
// These cover the Phase 4 acceptance rules that can be checked without a live shell:
// the pointer position clamped to the monitor, quick select 1-9, the pinned/recent
// sections, keyboard navigation, the paged render window, and the search debounce.
// The parts that need a real session (the popup actually opening at the cursor, key
// focus, no compositor frame drops) are checked live in docs/testing.md.

import system from 'system';
import * as popup from '../extension/popupLogic.js';
import {Debouncer} from '../extension/filters.js';

let failures = 0;
function check(cond, msg) {
    if (cond) {
        print(`ok: ${msg}`);
    } else {
        failures++;
        print(`FAIL: ${msg}`);
    }
}

// Canonical history row (ssstbbs).
const tuple = ['id-1', 'text/plain', 'a note', 1793944766140, false, false, 'org.gnome.TextEditor'];
const row = popup.rowFromTuple(tuple);
check(row.id === 'id-1' && row.primaryMime === 'text/plain' && row.preview === 'a note', 'tuple fields map by position');
check(row.sortTime === 1793944766140, 'sort_time is milliseconds');
check(row.pinned === false && row.hasThumbnail === false, 'booleans unpack as booleans');
check(row.sourceApp === 'org.gnome.TextEditor', 'source app is kept');

// Row badge types match the daemon's filters, so a row is in exactly one bucket.
check(popup.rowType(row) === 'text', 'text row');
check(popup.rowType({...row, primaryMime: 'image/png'}) === 'image', 'image row');
check(popup.rowType({...row, primaryMime: 'text/uri-list'}) === 'file', 'uri row is file, not text');
check(popup.rowType({...row, primaryMime: 'text/x-uri'}) === 'file', 'legacy URI row is file');
check(popup.rowType({...row, primaryMime: 'x-special/gnome-copied-files'}) === 'file', 'gnome-copied-files row is file');

// One line per row.
check(popup.displayPreview({...row, preview: 'a\nb\tc'}) === 'a b c', 'newlines collapse to one line');
const long = popup.displayPreview({...row, preview: 'x'.repeat(200)});
check(long.length === 120 && long.endsWith('\u2026'), 'preview truncated to the row width with an ellipsis');

// Quick select.
check(popup.quickSelectIndex(0) === 1, 'first row is quick select 1');
check(popup.quickSelectIndex(8) === 9, 'ninth row is quick select 9');
check(popup.quickSelectIndex(9) === null, 'tenth row has no quick select badge');
check(popup.digitIsQuickSelect(''), 'a digit is quick select while the entry is empty');
check(!popup.digitIsQuickSelect('1999'), 'a digit inside a query is part of the query, not a selection');

// The quick select digit reaches the popup as text, because Clutter.Text consumes printable
// keys before the key handler runs (measured live on 2026-11-06: pressing 1 with an empty
// entry produced a search for "1"). So the rule is applied to the change, not to the key event.
check(popup.quickSelectFromChange('', '1') === 1, 'an empty entry that becomes a single digit is a quick select key press');
check(popup.quickSelectFromChange('', '9') === 9, 'badge 9 is the last quick select key');
check(popup.quickSelectFromChange('', '0') === null, 'there is no badge 0');
check(popup.quickSelectFromChange('', 'a') === null, 'a letter is typing, not a selection');
check(popup.quickSelectFromChange('', '10') === null, 'two characters is not one key press');
check(popup.quickSelectFromChange('1999', '19991') === null, 'a digit inside a query is part of the query');

// Sections and the separator between them.
const rows = [
    {id: 'p1', pinned: true},
    {id: 'r1', pinned: false},
    {id: 'r2', pinned: false},
];
const sections = popup.groupSections(rows);
check(sections[0].name === 'pinned' && sections[0].rows.length === 1, 'pinned section first');
check(sections[1].name === 'recent' && sections[1].rows.length === 2, 'recent section second');
check(sections[1].separator === true, 'separator drawn when both sections have rows');
check(popup.groupSections([{id: 'r1', pinned: false}])[1].separator === false, 'no separator when nothing is pinned');
check(popup.groupSections([{id: 'p1', pinned: true}])[1].separator === false, 'no separator when nothing is recent');

// Pointer position clamped to the monitor work area.
const work = {x: 0, y: 0, width: 1920, height: 1080};
check(JSON.stringify(popup.clampToWorkArea(400, 300, 420, 480, work)) === JSON.stringify({x: 400, y: 300}),
    'a pointer inside the work area keeps the popup at the cursor');
const corner = popup.clampToWorkArea(1900, 1070, 420, 480, work);
check(corner.x === 1920 - 420 && corner.y === 1080 - 480, 'a pointer in the bottom-right corner is pulled inside');
const topLeft = popup.clampToWorkArea(-20, -20, 420, 480, work);
check(topLeft.x === 0 && topLeft.y === 0, 'a pointer past the top-left edge is pulled inside');
const inset = popup.clampToWorkArea(100, 100, 420, 480, {x: 50, y: 60, width: 1000, height: 900});
check(inset.x === 100 && inset.y === 100, 'clamping uses the work area origin, not the monitor origin');

// Keyboard navigation.
const nav = new popup.Navigator(5);
check(nav.index === 0, 'selection starts on the first row');
check(nav.move(1) === 1 && nav.move(1) === 2, 'Down moves the selection');
check(nav.move(-1) === 1, 'Up moves the selection back');
check(nav.move(10) === 4, 'Down clamps at the last row');
check(nav.home() === 0 && nav.end() === 4, 'Home and End jump to the ends');
check(nav.setIndex(3) === 3, 'quick select moves the selection to the badge row');
check(nav.setIndex(99) === 4, 'setIndex clamps past the end');
check(new popup.Navigator(0).move(1) === -1, 'an empty list has no selection');
nav.setCount(2);
check(nav.index === 1, 'shrinking the list keeps the selection inside it');

// Paged rendering: at most 20 rows queried on demand, and the window slides with the
// selection so navigation can reach past the loaded page.
const fetched = [];
const pager = new popup.PageWindow((req) => fetched.push(req));
check(pager.ensure(0) === true, 'the first visible row triggers the first page');
check(JSON.stringify(fetched[0]) === JSON.stringify({offset: 0, limit: 20}), 'first page is 20 rows from offset 0');
pager.append(Array.from({length: 20}, (_, i) => ({id: `row-${i}`})));
check(pager.count === 20, 'the pager keeps every loaded row, not just the window');
check(pager.windowFor(19).length === popup.MAX_VISIBLE, 'rendering is capped at 20 rows');
check(pager.start === 0 && pager.windowFor(19)[0].id === 'row-0', 'the first window starts at row 0');
check(pager.ensure(19) === false, 'navigation inside the loaded rows does not cross the bus');
check(pager.ensure(20) === true, 'reaching past the loaded rows fetches the next page');
check(JSON.stringify(fetched[1]) === JSON.stringify({offset: 20, limit: 20}), 'second page starts where the first ended');
pager.append(Array.from({length: 20}, (_, i) => ({id: `row-${20 + i}`})));
check(pager.windowFor(20).length === 20 && pager.windowFor(20)[0].id === 'row-20' && pager.start === 20,
    'the window slides so the selection is inside it');
check(pager.windowFor(39)[0].id === 'row-20' && pager.start === 20, 'the window does not slide past the loaded rows');
check(pager.windowFor(5).length === 20 && pager.windowFor(5)[0].id === 'row-0' && pager.start === 0,
    'a selection inside the first page renders that page');
check(popup.reachableCount(pager) === pager.count + 1, 'navigation can reach the row that is not loaded yet, which is what fetches page two');
pager.append([]);
check(pager.exhausted === true && pager.ensure(40) === false, 'an empty page ends the list');
check(popup.reachableCount(pager) === pager.count, 'an exhausted pager has no reachable row past the loaded ones');

// The path the UI takes for each key press: move the selection, then ask the pager for the
// page it reached. Clamping the selection to the loaded rows is what made the second page
// unreachable live on 2026-11-06.
const walk = new popup.PageWindow((req) => fetched.push(req));
walk.append(Array.from({length: 20}, (_, i) => ({id: `n-${i}`})));
const walkNav = new popup.Navigator(popup.reachableCount(walk));
let walkRequests = 0;
for (let i = 0; i < popup.MAX_VISIBLE; i++) {
    if (walk.ensure(walkNav.move(1))) walkRequests++;
}
check(walkNav.index === popup.MAX_VISIBLE, 'the 20th Down press lands past the loaded page');
check(walkRequests === 1, 'reaching past the loaded rows asked for exactly one page');

// A full second page slides the window without changing its size, which is why the UI compares
// the window start rather than the number of rows.
const full = new popup.PageWindow(() => {});
full.append(Array.from({length: 40}, (_, i) => ({id: `f-${i}`})));
check(full.windowFor(19).length === 20 && full.start === 0, 'row 19 is in the first window');
check(full.windowFor(20).length === 20 && full.start === 20, 'row 20 is in the second window, same size');

// Search: a burst of keystrokes inside the window produces one query.
const queries = [];
const scheduler = {arm: (fn, delayMs) => { scheduler._pending = {fn, delayMs}; return 1; }, cancel: () => { scheduler._pending = null; }};
const debouncer = new Debouncer(popup.SEARCH_DEBOUNCE_MS, (q) => queries.push(q), scheduler);
for (const q of ['c', 'cl', 'cli']) debouncer.schedule(q);
check(scheduler._pending.delayMs === popup.SEARCH_DEBOUNCE_MS, 'search debounce window is 20 ms');
scheduler._pending.fn();
check(queries.length === 1 && queries[0] === 'cli', 'typing a word sends one query, not one per keystroke');

if (failures > 0) {
    print(`\n${failures} popup logic check(s) failed`);
    system.exit(1);
}
print('\nall popup logic checks passed');
system.exit(0);

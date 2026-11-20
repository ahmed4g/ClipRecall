// Headless unit tests for the extension's pure logic. Run with:
//   gjs -m tests/testFilters.js
//
// Covers the Phase 3 acceptance rules that can be checked without a live shell:
// password manager hint detection, MIME allowlist, 50 ms debounce coalescing,
// the size limit, and zero-copy GVariant marshalling (asserting zero Array.from
// calls on the hot path). Live end-to-end capture is checked separately in a
// real GNOME Wayland session (docs/testing.md).

import system from 'system';
import GLib from 'gi://GLib';
import * as filters from '../extension/filters.js';
import {submitBody} from '../extension/dbus.js';

let failures = 0;
function check(cond, msg) {
    if (cond) {
        print(`ok: ${msg}`);
    } else {
        failures++;
        print(`FAIL: ${msg}`);
    }
}

// Password manager hints.
check(filters.isSensitive(['text/plain', 'application/x-kde-passwordManagerHint']), 'hint MIME detected');
check(filters.isSensitive(['x-kde-passwordManagerHint']), 'unprefixed hint variant detected');
check(filters.isSensitive(['org.nspasteboard.ConcealedType']), 'concealed type detected');
check(filters.isSensitive(['application/x-gnome-concealed']), 'gnome concealed detected');
check(!filters.isSensitive(['text/plain', 'text/html']), 'ordinary copy is not sensitive');

// MIME allowlist.
const allowed = filters.allowedMimes(['text/plain', 'text/html', 'application/pdf', 'image/png']);
check(JSON.stringify(allowed) === JSON.stringify(['text/plain', 'text/html', 'image/png']), 'allowlist drops unlisted MIME');
check(filters.allowedMimes(['UTF8_STRING', 'text/uri-list', 'text/x-uri', 'x-special/gnome-copied-files']).length === 4,
    'allowlist covers shell and file URIs');

const utf8Text = new TextEncoder().encode('café 日本語 👋');
const invalidUtf8 = new Uint8Array([0xff]);
check(filters.pasteMime('text/plain', utf8Text) === 'text/plain;charset=utf-8',
    'restored UTF-8 text advertises its encoding');
check(filters.pasteMime('text/plain', invalidUtf8) === 'text/plain',
    'restored non-UTF-8 text keeps the original MIME');
check(filters.pasteMime('text/plain;charset=utf-8', utf8Text) === 'text/plain;charset=utf-8',
    'restored text with an explicit encoding keeps its MIME');
check(filters.pasteMime('image/png', invalidUtf8) === 'image/png',
    'restored binary payload keeps its MIME');

// Size limit.
check(filters.exceedsSize(17 * 1024 * 1024, 16 * 1024 * 1024), 'oversized copy rejected');
check(!filters.exceedsSize(16 * 1024 * 1024, 16 * 1024 * 1024), 'exactly at the limit is accepted');

// Debounce: three events inside the window produce one flush with the latest value.
const armed = [];
let armCalls = 0;
const scheduler = {
    arm: (fn, delayMs) => {
        armCalls++;
        armed.push({fn, delayMs});
        return armed.length;
    },
    cancel: (id) => {
        armed[id - 1] = null;
    },
};
const fired = [];
const debouncer = new filters.Debouncer(50, (v) => fired.push(v), scheduler);
debouncer.schedule('a');
debouncer.schedule('b');
debouncer.schedule('c');
check(armCalls === 3 && armed.filter(Boolean).length === 1, 'coalesced to a single pending timer');
check(armed.filter(Boolean)[0].delayMs === 50, 'debounce window is 50 ms');
armed.filter(Boolean)[0].fn();
check(fired.length === 1 && fired[0] === 'c', 'fires once with the latest event');

// Zero-copy marshalling: the hot path must not copy bytes through Array.from.
let arrayFromCalls = 0;
const originalFrom = Array.from;
Array.from = function (...args) {
    arrayFromCalls++;
    return originalFrom.apply(this, args);
};
const body = submitBody({'text/plain': new GLib.Bytes([104, 105])}, 'org.gnome.TextEditor');
Array.from = originalFrom;
check(arrayFromCalls === 0, 'marshalling makes zero Array.from calls');

const [payloads, sourceApp] = body.deepUnpack();
check(sourceApp === 'org.gnome.TextEditor', 'source app carried in the body');
check(Object.keys(payloads).length === 1 && payloads['text/plain'].length === 2, 'payload dictionary round-trips');
check(body.print(true).startsWith('(') && body.print(true).includes('text/plain'), 'body is the documented (a{say}s) tuple');

// StatusChanged states: which ones invalidate the cached settings and exclusion list.
check(filters.statusAction('paused') === 'pause', 'paused state pauses ingest');
check(filters.statusAction('resumed') === 'resume', 'resumed state resumes ingest');
check(filters.statusAction('exclusions-changed') === 'refresh', 'exclusion change invalidates the cache');
check(filters.statusAction('settings-changed') === 'refresh', 'settings change invalidates the cache');
check(filters.statusAction('connected') === 'ignore', 'other states do not trigger a bus round trip');

if (failures > 0) {
    print(`\n${failures} test(s) failed`);
    // Non-zero exit so the Makefile gate fails.
    system.exit(1);
}
print('\nall extension filter tests passed');

// Live check of the bridge against a running daemon on the session bus:
//   gjs -m tests/testBridgeLive.js
//
// This is not part of `make test`: it needs the daemon to be running
// (target/release/clipboard-manager-daemon, or D-Bus activation). It skips with
// exit 0 when the bus name has no owner, so it never fails a headless run.
//
// What it proves, and what it does not:
// - Proves the proxy construct properties and the name watcher used by dbus.js are
//   the ones this gjs/GLib actually exposes, and that the marshalled SubmitItem body
//   reaches the live daemon and writes a row.
// - Proves the cached exclusion list is invalidated by the live StatusChanged signal,
//   which is what makes the extension's excluded-app filter work after a settings change.
// - Measures the client -> daemon -> SQLite latency at the client.
// - Does not prove the shell observer: `owner-changed` and `read_async` need the
//   extension loaded in a live GNOME Shell session.
//
// It writes one row into the real history database and adds then removes one
// excluded app. The daemon must not be paused, otherwise SubmitItem legitimately
// returns an empty id.

import system from 'system';
import GLib from 'gi://GLib';
import Gio from 'gi://Gio';
import {DaemonBridge, DAEMON_NAME, DAEMON_PATH, DAEMON_IFACE} from '../extension/dbus.js';

let failures = 0;
let skipped = false;
let invalidationChecked = false;

function check(cond, msg) {
    if (cond) {
        print(`ok: ${msg}`);
    } else {
        failures++;
        print(`FAIL: ${msg}`);
    }
}

const loop = new GLib.MainLoop(null, false);
let arrayFromCalls = 0;
const originalFrom = Array.from;
Array.from = function (...args) {
    arrayFromCalls++;
    return originalFrom.apply(this, args);
};

const APP = 'org.clipboardmanager.LiveCheck';

// Separate proxy for the method the bridge does not expose, so the test acts exactly
// like the settings UI would.
const admin = new Gio.DBusProxy({
    gConnection: Gio.DBus.session,
    gBusType: Gio.BusType.SESSION,
    gName: DAEMON_NAME,
    gObjectPath: DAEMON_PATH,
    gInterfaceName: DAEMON_IFACE,
});

function finish() {
    loop.quit();
}

function phase2(bridge) {
    const beforeRefresh = bridge.excludedApps;
    bridge.refresh();

    const started = GLib.get_monotonic_time();
    const payload = {'text/plain': new GLib.Bytes(new TextEncoder().encode('live bridge check'))};

    // GDBus pipelines calls, so the GetSettings and ListExcludedApps replies issued by
    // refresh() arrive before the SubmitItem reply below.
    bridge.submit(payload, APP, (id) => {
        const elapsedUs = GLib.get_monotonic_time() - started;

        check(id !== '', 'daemon returned a row id (the insert ran)');
        check(elapsedUs < 100000, `client -> daemon -> SQLite in ${elapsedUs} us (gate: < 100 ms)`);
        check(arrayFromCalls === 0, 'live submit path makes zero Array.from calls');
        check(bridge.maxItemBytes === 16 * 1024 * 1024, 'live settings read: max_item_bytes is the documented default');
        check(bridge.excludedApps !== beforeRefresh, 'exclusion list replaced by the live ListExcludedApps reply');

        // Phase 3: the cached exclusion list must follow a change announced on the bus.
        admin.call('AddExcludedApp', new GLib.Variant('(s)', [APP]), Gio.DBusCallFlags.NONE, -1, null,
            (proxy, res) => {
                try {
                    proxy.call_finish(res);
                } catch (err) {
                    failures++;
                    print(`FAIL: AddExcludedApp failed: ${err.message}`);
                    finish();
                }
            });
    });
}

const bridge = new DaemonBridge({
    onConnectionChanged: (state) => {
        if (state === 'lost') {
            skipped = true;
            print('skip: org.clipboardmanager.Daemon has no owner on the session bus');
            finish();
            return;
        }
        check(true, 'name watcher reports the daemon (bus_watch_name_on_connection)');
        check(bridge.connected, 'bridge marked connected by the watcher');
        phase2(bridge);
    },

    onStatusChanged: (state) => {
        if (state !== 'exclusions-changed' || invalidationChecked) return;
        invalidationChecked = true;
        check(true, 'StatusChanged("exclusions-changed") reached the live bridge');

        // The refresh the signal triggers is a bus round trip, so give the reply a turn
        // of the loop before asserting the cache changed.
        GLib.timeout_add(GLib.PRIORITY_DEFAULT, 500, () => {
            check(bridge.excludedApps.has(APP), 'cached exclusion list updated by the live signal');
            admin.call('RemoveExcludedApp', new GLib.Variant('(s)', [APP]), Gio.DBusCallFlags.NONE, -1, null,
                (proxy, res) => {
                    try {
                        proxy.call_finish(res);
                    } catch (err) {
                        print(`note: RemoveExcludedApp failed: ${err.message}`);
                    }
                    finish();
                });
            return GLib.SOURCE_REMOVE;
        });
    },
});

GLib.timeout_add(GLib.PRIORITY_DEFAULT, 8000, () => {
    if (!skipped) {
        failures++;
        print('FAIL: the live check did not complete within 8 s');
    }
    finish();
    return GLib.SOURCE_REMOVE;
});

loop.run();
bridge.destroy();

if (skipped) system.exit(0);
if (failures > 0) {
    print(`\n${failures} live check(s) failed`);
    system.exit(1);
}
print('\nlive bridge check passed');
system.exit(0);

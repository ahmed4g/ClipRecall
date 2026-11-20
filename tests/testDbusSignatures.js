// Pins the client-side D-Bus signatures against the canonical interface definition.
// Run with: gjs -m tests/testDbusSignatures.js
//
// data/org.clipboardmanager.Daemon.xml is the single source of truth, and daemon/tests/dbus_spec.rs
// enforces it on the daemon side. Nothing checked the extension side until a live search failed on
// 2026-11-06 with "Expected type string for argument 'string' but got type number": dbus.js built
// SearchHistory as (is) while the interface declares query then limit, (si). The daemon answered the
// error, so the call failed at runtime rather than at test time — which is why the client side is
// now checked against the same file.

import system from 'system';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import {DaemonBridge} from '../extension/dbus.js';

let failures = 0;

function check(cond, msg) {
    if (cond) {
        print(`ok: ${msg}`);
    } else {
        failures++;
        print(`FAIL: ${msg}`);
    }
}

function readSource(path) {
    const bytes = Gio.File.new_for_path(path).load_contents(null)[1];
    return new TextDecoder().decode(bytes);
}

const xml = readSource('data/org.clipboardmanager.Daemon.xml');

// method name -> {in: types in declaration order, out: types in declaration order}
const methods = {};
for (const block of xml.matchAll(/<method name="(\w+)">([\s\S]*?)<\/method>/g)) {
    const inTypes = [];
    const outTypes = [];
    for (const arg of block[2].matchAll(/<arg type="([^"]+)"[^>]*direction="(in|out)"\/>/g)) {
        if (arg[2] === 'in') inTypes.push(arg[1]);
        else outTypes.push(arg[1]);
    }
    methods[block[1]] = {in: inTypes.join(''), out: outTypes.join('')};
}

check(Object.keys(methods).length > 0, 'the interface definition parsed');

const bridge = readSource('extension/dbus.js');

// Helper functions that build a body, e.g. submitBody() for SubmitItem.
const helpers = {};
for (const fn of bridge.matchAll(/export function (\w+)\([^)]*\)\s*\{\s*return new GLib\.Variant\('\(([^']*)\)'/g)) {
    helpers[fn[1]] = fn[2];
}

// Every call the bridge makes must use the signature the interface declares.
const calls = [...bridge.matchAll(/this\._call\('(\w+)',\s*(null|new GLib\.Variant\('\(([^']*)\)'|([A-Za-z_$][\w$]*)\()/g)];
check(calls.length > 0, 'the bridge calls were found to check');

for (const call of calls) {
    const name = call[1];
    let signature;
    if (call[2] === 'null') signature = '';
    else if (call[3]) signature = call[3];
    else signature = helpers[call[4]];

    const declared = methods[name];
    if (!declared) {
        failures++;
        print(`FAIL: ${name} is called by the bridge but not declared in the interface`);
        continue;
    }
    check(signature === declared.in, `${name} body is (${signature}), the interface declares (${declared.in})`);
}

// The reply shapes the extension unpacks.
check(methods.GetHistory.out === 'a(ssstbbs)', 'GetHistory replies with the canonical row tuple array');
check(methods.SearchHistory.out === 'a(ssstbbs)', 'SearchHistory replies with the same tuple array');
check(methods.GetItemContent.out === 'say', 'GetItemContent replies with mime then bytes');

const mime = 'application/octet-stream';
const bytes = new Uint8Array([0, 127, 255]);
const reply = new GLib.Variant(`(${methods.GetItemContent.out})`, [mime, bytes]);
const stub = {_call: (_method, _body, onReply) => onReply(reply)};
let content;
DaemonBridge.prototype.getContent.call(stub, 'test-id', mime, (value) => { content = value; });
check(Array.isArray(content) && content[0] === mime &&
    content[1] instanceof Uint8Array && content[1].length === bytes.length &&
    bytes.every((byte, i) => content[1][i] === byte),
    'GetItemContent delivers the MIME and unmodified payload bytes to the caller');

const thumbnailReply = new GLib.Variant(`(${methods.GetThumbnail.out})`, [bytes]);
DaemonBridge.prototype.getThumbnail.call({_call: (_m, _b, receive) => receive(thumbnailReply)}, 'id', png => {
    check(png instanceof GLib.Bytes && png.equal(GLib.Bytes.new(bytes)),
        'GetThumbnail delivers the original PNG bytes to the icon loader');
});

for (const [payloads, preferred] of [
    [{'text/plain': bytes, 'text/html': bytes, 'text/rtf': bytes}, 'text/html'],
    [{'text/plain': bytes, 'text/rtf': bytes}, 'text/rtf'],
    [{'text/plain': bytes}, null],
]) {
    const payloadsReply = new GLib.Variant(`(${methods.GetItemPayloads.out})`, [payloads]);
    DaemonBridge.prototype.getRichText.call({_call: (_m, _b, receive) => receive(payloadsReply)}, 'id', rich => {
        check(preferred ? rich?.[0] === preferred && bytes.every((byte, index) => rich[1][index] === byte) : rich === null,
            `explicit rich restore chooses ${preferred ?? 'no rich format'}`);
    });
}

const actionCases = [
    ['pinItem', 'PinItem', ['test-id'], [true]],
    ['unpinItem', 'UnpinItem', ['test-id'], [false]],
    ['deleteItem', 'DeleteItem', ['test-id', false], [false]],
    ['deleteItem', 'DeleteItem', ['test-id', true], [true]],
    ['clearHistory', 'ClearHistory', [false], [7]],
    ['clearHistory', 'ClearHistory', [true], [0]],
    ['setPaused', 'SetPaused', [true], []],
    ['setPaused', 'SetPaused', [false], []],
    ['addExcludedApp', 'AddExcludedApp', ['test-app'], []],
    ['removeExcludedApp', 'RemoveExcludedApp', ['test-app'], []],
    ['exportHistory', 'ExportHistory', ['/tmp/test.json', 'json'], [3]],
];

for (const [bridgeMethod, dbusMethod, input, output] of actionCases) {
    const {in: inputTypes, out: outputTypes} = methods[dbusMethod];
    const actionReply = new GLib.Variant(`(${outputTypes})`, output);
    let sent;
    let fail = false;
    const actionStub = {
        _call(method, body, onReply, onError) {
            sent = [method, body.get_type_string(), body.deepUnpack()];
            if (fail) onError();
            else onReply(actionReply);
        },
    };

    let received;
    DaemonBridge.prototype[bridgeMethod].call(actionStub, ...input, (...args) => { received = args; });
    check(JSON.stringify(sent) === JSON.stringify([dbusMethod, `(${inputTypes})`, input]),
        `${bridgeMethod} sends ${JSON.stringify(input)} in the declared D-Bus order`);
    const completion = outputTypes ? output : [true];
    check(JSON.stringify(received) === JSON.stringify(completion),
        `${bridgeMethod} delivers ${JSON.stringify(completion)} to the caller`);
    DaemonBridge.prototype[bridgeMethod].call(actionStub, ...input);

    fail = true;
    received = undefined;
    DaemonBridge.prototype[bridgeMethod].call(actionStub, ...input, (...args) => { received = args; });
    const failure = ['ClearHistory', 'ExportHistory'].includes(dbusMethod) ? [null] : [false];
    check(JSON.stringify(received) === JSON.stringify(failure),
        `${bridgeMethod} delivers ${JSON.stringify(failure)} on transport failure`);
}

for (const [key, signature, value] of [['max_entries', 'i', 4500], ['filter_passwords', 'b', false]]) {
    DaemonBridge.prototype.setSetting.call({_call(method, body, complete) {
        const [sentKey, sentValue] = body.deepUnpack();
        check(method === 'SetSetting' && sentKey === key && sentValue.get_type_string() === signature &&
            sentValue.deepUnpack() === value, `SetSetting sends a typed ${key} value`);
        complete();
    }}, key, new GLib.Variant(signature, value), ok => check(ok, `SetSetting acknowledges ${key}`));
}

// A pause status is a cached view of daemon state. Exercise the signal and snapshot
// paths without constructing a proxy or connecting to the user's session bus.
const statuses = [];
const exclusionsReplies = [];
const settingsReplies = [];
const pauseBridge = Object.assign(Object.create(DaemonBridge.prototype), {
    paused: false,
    settingsReady: false,
    _pauseRevision: 0,
    _settingsRevision: 0,
    _onStatusChanged: (state) => statuses.push(state),
    _call(method, _body, onReply) {
        if (method === 'ListExcludedApps') exclusionsReplies.push(onReply);
        else if (method === 'GetSettings') settingsReplies.push(onReply);
    },
});
const replyWithSettings = (settings) => new GLib.Variant('(a{sv})', [settings]);
const pausedSettings = (paused) => replyWithSettings({paused: new GLib.Variant('b', paused)});
const exclusionReply = new GLib.Variant('(as)', [['excluded-app']]);
function deliverExclusions() {
    exclusionsReplies.shift()(exclusionReply);
}

pauseBridge.refresh();
check(!pauseBridge.settingsReady && settingsReplies.length === 0,
    'capture stays blocked until the exclusion list arrives');
deliverExclusions();
check(!pauseBridge.settingsReady && pauseBridge.excludedApps.has('excluded-app'),
    'capture remains blocked while waiting for GetSettings');
settingsReplies.shift()(pausedSettings(true));
check(pauseBridge.settingsReady && pauseBridge.paused && statuses.join(',') === 'paused',
    'GetSettings opens capture readiness and synchronizes pause before any status signal arrives');

pauseBridge.refresh();
deliverExclusions();
settingsReplies.shift()(replyWithSettings({}));
check(pauseBridge.paused && statuses.join(',') === 'paused',
    'GetSettings preserves the last pause state when an old daemon omits paused');

pauseBridge.refresh();
deliverExclusions();
pauseBridge._handleStatusChanged('resumed');
settingsReplies.shift()(pausedSettings(true));
check(!pauseBridge.paused && statuses.join(',') === 'paused,resumed',
    'an older GetSettings reply cannot override a newer pause status signal');

pauseBridge.refresh();
deliverExclusions();
pauseBridge.refresh();
deliverExclusions();
settingsReplies.pop()(pausedSettings(false));
settingsReplies.shift()(pausedSettings(true));
check(!pauseBridge.paused && statuses.join(',') === 'paused,resumed',
    'an older GetSettings reply cannot override a newer snapshot or duplicate status');

pauseBridge.settingsReady = false;
pauseBridge.refresh();
deliverExclusions();
settingsReplies.shift()(replyWithSettings({}));
check(pauseBridge.settingsReady && !pauseBridge.paused && statuses.at(-1) === 'settings-ready',
    'a new connection to an old daemon becomes ready and notifies the indicator');

// A mutation completing after daemon replacement must release its UI controls
// without reporting success from a daemon instance the UI no longer uses.
const pendingCalls = [];
const proxyStub = {
    call(_method, _body, _flags, _timeout, _cancellable, callback) {
        pendingCalls.push(callback);
    },
    call_finish(result) {
        return result;
    },
};
const transportBridge = Object.assign(Object.create(DaemonBridge.prototype), {
    _proxy: proxyStub,
    _daemonGeneration: 0,
    _watcherId: null,
});
const pinReply = new GLib.Variant('(b)', [true]);
let mutationResult;
transportBridge.pinItem('test-id', (value) => { mutationResult = value; });
transportBridge._daemonGeneration++;
pendingCalls.shift()(proxyStub, pinReply);
check(mutationResult === false, 'a reply from an older daemon generation reports failure');

mutationResult = undefined;
transportBridge.pinItem('test-id', (value) => { mutationResult = value; });
transportBridge.destroy();
pendingCalls.shift()(proxyStub, pinReply);
check(mutationResult === undefined, 'a destroyed bridge drops pending UI callbacks');

// The row tuple is read by position, so the field names have to follow the tuple order.
const tupleTypes = methods.GetHistory.out.slice(2, -1).split('');
const rowFields = [...readSource('extension/popupLogic.js').matchAll(/tuple\[(\d+)\]/g)].map((m) => parseInt(m[1], 10));
check(rowFields.length === tupleTypes.length, `rowFromTuple reads all ${tupleTypes.length} tuple fields`);
check(JSON.stringify(rowFields) === JSON.stringify(tupleTypes.map((_, i) => i)),
    'rowFromTuple reads the fields in tuple order, not by name');

// t and b are numbers and booleans after deepUnpack, so the UI must not treat them as strings.
const popupLogic = readSource('extension/popupLogic.js');
check(/sortTime: Number\(tuple\[(\d+)\]\)/.test(popupLogic) && tupleTypes[3] === 't',
    'the t field (sort_time) is read as a number');
check(/pinned: Boolean\(tuple\[(\d+)\]\)/.test(popupLogic) && tupleTypes[4] === 'b',
    'the b field (pinned) is read as a boolean');
check(/hasThumbnail: Boolean\(tuple\[(\d+)\]\)/.test(popupLogic) && tupleTypes[5] === 'b',
    'the b field (has_thumbnail) is read as a boolean');

// The live gate parses the same tuple by position, so it must list the fields in the same order.
const gate = readSource('tests/verifyPopup.py');
const gateFields = (gate.match(/ROW_FIELDS = \(([^)]*)\)/) || ['', ''])[1]
    .split(',')
    .map((f) => f.trim().replace(/^["']|["']$/g, ''))
    .filter(Boolean);
const tupleNames = ['id', 'primary_mime', 'preview', 'sort_time', 'pinned', 'has_thumbnail', 'source_app'];
check(JSON.stringify(gateFields) === JSON.stringify(tupleNames),
    'the live gate reads the tuple fields in the order the interface declares them');

if (failures > 0) {
    print(`\n${failures} D-Bus signature check(s) failed`);
    system.exit(1);
}
print('\nD-Bus signature checks passed');
system.exit(0);

// Verifies the Meta read path the observer depends on, headless (no shell):
//   GI_TYPELIB_PATH=/usr/lib64/mutter-18 gjs -m tests/testReadPath.js
//
// Two facts this test pins down, both of which the observer had wrong before:
// - `Meta.Selection.get_selection()` does not exist; the selection is reached through
//   the display (`global.display.get_selection()`).
// - `read_finish()` returns a GInputStream, not GBytes, so the payload has to be read
//   from the stream (`read_bytes_async`) before it can go into the `ay` variant.
//
// It skips with exit 0 when the Meta typelib is not reachable from gjs.

import system from 'system';
import GLib from 'gi://GLib';

let failures = 0;

function check(cond, msg) {
    if (cond) {
        print(`ok: ${msg}`);
    } else {
        failures++;
        print(`FAIL: ${msg}`);
    }
}

let Meta;
try {
    const mod = await import('gi://Meta');
    Meta = mod.default || mod;
} catch (err) {
    print('skip: gi://Meta is not importable here (set GI_TYPELIB_PATH to the mutter typelib)');
    system.exit(0);
}

check(typeof Meta.Display.prototype.get_selection === 'function',
    'the selection is reached through Meta.Display.get_selection');
check(Meta.Selection.get_selection === undefined,
    'Meta.Selection.get_selection does not exist (the observer must not call it)');
check(Meta.SelectionType.SELECTION_CLIPBOARD !== undefined,
    'SelectionType.SELECTION_CLIPBOARD is the clipboard selection');

check(typeof Meta.Selection.prototype.transfer_async === 'function' &&
    typeof Meta.Selection.prototype.transfer_finish === 'function',
    'Mutter exposes bounded transfers through the selection manager');

const content = new TextEncoder().encode('hello world');
const source = Meta.SelectionSourceMemory.new('text/plain', GLib.Bytes.new(content));

check(JSON.stringify(source.get_mimetypes()) === JSON.stringify(['text/plain']),
    'get_mimetypes reports the source MIME');

const loop = new GLib.MainLoop(null, false);

source.read_async('text/plain', null, (src, result) => {
    const stream = src.read_finish(result);

    check(stream !== null && typeof stream.read_bytes_async === 'function',
        'read_finish returns a stream, not GBytes');
    check(stream instanceof GLib.Bytes === false,
        'read_finish result is not GLib.Bytes (the old code assumed it was)');

    stream.read_bytes_async(64, GLib.PRIORITY_DEFAULT, null, (input, result) => {
        try {
            const bytes = input.read_bytes_finish(result);

            check(bytes instanceof GLib.Bytes, 'read_bytes_finish yields GLib.Bytes');
            check(bytes.get_size() === content.length, 'payload size matches the copied content');
            check(new TextDecoder().decode(bytes.toArray()) === 'hello world',
                'payload content matches');

            // The value must be the GLib.Bytes itself: wrapping it in an array marshals a
            // single byte instead of the payload.
            const body = new GLib.Variant('(a{say}s)', [{ 'text/plain': bytes }, 'org.gnome.TextEditor']);
            const unpacked = body.deep_unpack();
            check(unpacked[0]['text/plain'].length === content.length,
                'the (a{say}s) body round-trips with the full payload');
            check(new TextDecoder().decode(unpacked[0]['text/plain']) === 'hello world',
                'the round-tripped payload is the copied content');
            check(unpacked[1] === 'org.gnome.TextEditor', 'source app travels with the body');
        } catch (err) {
            failures++;
            print(`FAIL: read path error: ${err.message}`);
        }

        loop.quit();
    });
});

GLib.timeout_add(GLib.PRIORITY_DEFAULT, 5000, () => {
    failures++;
    print('FAIL: the read callback did not run');
    loop.quit();
    return GLib.SOURCE_REMOVE;
});

loop.run();

if (failures > 0) {
    print(`\n${failures} read path check(s) failed`);
    system.exit(1);
}
print('\nread path checks passed');
system.exit(0);

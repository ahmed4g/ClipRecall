// Parses the extension sources with the engine GNOME Shell actually uses.
//   gjs -m tests/testSyntax.js
//
// A brace imbalance in extension.js is invisible until the shell loads the module, so
// checking it here is the point. V8 (node --check) accepted the file that SpiderMonkey
// rejected on 2026-11-06, so this uses gjs itself rather than node.
//
// Imports are stripped and the module body is parsed through new Function(), which parses
// without executing anything.

import system from 'system';
import Gio from 'gi://Gio';

const FILES = ['extension/extension.js', 'extension/filters.js', 'extension/dbus.js',
    'extension/popup.js', 'extension/popupLogic.js', 'extension/indicator.js', 'extension/prefs.js'];

function parseOnly(path) {
    const bytes = Gio.File.new_for_path(path).load_contents(null)[1];
    const source = new TextDecoder().decode(bytes)
        .split('\n')
        .filter((line) => !line.startsWith('import '))
        .join('\n')
        // export is module-level syntax; strip it so the body can be parsed as a function body.
        .replace('export default ', 'const parsed = ')
        .replace(/^export /gm, '');
    // new Function parses the body; it is never called, so nothing runs.
    new Function(source);
}

let failures = 0;

for (const file of FILES) {
    try {
        parseOnly(file);
        print(`ok: ${file} parses`);
    } catch (err) {
        failures++;
        print(`FAIL: ${file}: ${err.message}`);
    }
}

if (failures > 0) system.exit(1);
print('\nsyntax checks passed');
system.exit(0);

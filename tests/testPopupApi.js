// Verifies the St/Clutter API the popup depends on, headless (no shell):
//   LD_LIBRARY_PATH=/usr/lib64/gnome-shell GI_TYPELIB_PATH=/usr/lib64/gnome-shell:/usr/lib64/mutter-18 \
//     gjs -m tests/testPopupApi.js
//
// GNOME Shell 50 (mutter 18) renamed or removed several Clutter APIs the popup code
// would otherwise assume, so these facts are pinned here rather than discovered when the
// shell loads the extension:
// - Clutter.Actor has add_child(); add_actor() is gone.
// - Clutter.Event has get_key_symbol(); get_keyval() is gone.
// - 'text-changed' is a signal of Clutter.Text, not of St.Entry, so the popup connects it
//   on the entry's text actor (which is what the shell's own entry code does).
// - St.Entry has no `hint` property; the placeholder is the `hint-text` property.
// - A keybinding must be registered through Main.wm.addKeybinding(name, settings, flags,
//   modes, handler): ShellWM's filter-keybinding handler drops bindings not in Main.wm's
//   allow list, so global.display.add_keybinding() alone never fires.
//
// It skips with exit 0 when the St typelib is not reachable from gjs, so a headless run on
// a machine without gnome-shell installed does not fail.

import system from 'system';
import Gio from 'gi://Gio';
import GObject from 'gi://GObject';

let failures = 0;

function check(cond, msg) {
    if (cond) {
        print(`ok: ${msg}`);
    } else {
        failures++;
        print(`FAIL: ${msg}`);
    }
}

let St, Clutter;
try {
    St = (await import('gi://St')).default;
    Clutter = (await import('gi://Clutter')).default;
} catch (err) {
    print(`skip: gi://St is not importable here (${err.message})`);
    system.exit(0);
}

function hasMethod(ctor, name) {
    return typeof ctor.prototype[name] === 'function';
}

function hasSignal(ctor, name) {
    return GObject.signal_lookup(name, ctor.$gtype) !== 0;
}

// Containers: add_child, not the removed add_actor.
check(hasMethod(Clutter.Actor, 'add_child'), 'Clutter.Actor.add_child exists');
check(!hasMethod(Clutter.Actor, 'add_actor'), 'Clutter.Actor.add_actor does not exist (the popup must not call it)');
check(hasMethod(Clutter.Actor, 'destroy_all_children'), 'the row list is cleared with destroy_all_children');
check(hasMethod(Clutter.Actor, 'set_position'), 'the popup is positioned with set_position');
check(hasMethod(Clutter.Actor, 'grab_key_focus'), 'key focus is taken with grab_key_focus');
check(hasMethod(Clutter.Actor, 'get_transformed_position') && hasMethod(Clutter.Actor, 'get_transformed_size'),
    'outside clicks can be checked against the actual popup rectangle');
check(hasMethod(Clutter.Actor, 'has_allocation') && hasMethod(Clutter.Actor, 'get_allocation_box') &&
      !!Clutter.Actor.find_property('allocation'), 'row visibility can be checked after allocation');

// Keys: get_key_symbol, not the removed get_keyval.
check(hasMethod(Clutter.Event, 'get_key_symbol'), 'Clutter.Event.get_key_symbol exists');
check(!hasMethod(Clutter.Event, 'get_keyval'), 'Clutter.Event.get_keyval does not exist (the popup must not call it)');
check(hasMethod(Clutter.Event, 'get_state'), 'modifiers are read with get_state');
check(hasMethod(Clutter.Event, 'get_flags') && Clutter.EventFlags.FLAG_REPEATED !== undefined,
    'held action keys can be distinguished from initial key presses');
check(hasMethod(Clutter.Event, 'get_coords'), 'mouse coordinates are available on the click event');

// Signals the popup connects to.
check(hasSignal(St.Button, 'clicked'), 'St.Button emits clicked');
check(hasSignal(St.Widget, 'key-press-event'), 'the popup handles key-press-event');
check(hasSignal(St.Widget, 'button-press-event'), 'the popup handles button-press-event');
check(hasSignal(Clutter.Actor, 'captured-event'), 'action shortcuts can be handled before the search entry');
check(hasSignal(St.Widget, 'destroy'), 'the popup connects to destroy');

// The entry: text-changed belongs to the text actor, not to St.Entry itself.
check(hasSignal(Clutter.Text, 'text-changed'), 'text-changed is a Clutter.Text signal');
check(!hasSignal(St.Entry, 'text-changed'), 'St.Entry does not emit text-changed (the popup connects it on the text actor)');
check(hasMethod(St.Entry, 'get_clutter_text'), 'the entry text actor is reached with get_clutter_text');
check(hasMethod(St.Entry, 'get_text') && hasMethod(St.Entry, 'set_text'), 'the query is read with get_text/set_text');
check(hasMethod(St.Entry, 'set_hint_text'), 'the placeholder is the hint-text property, not hint');

// Style classes are St.Widget methods, and St.Button reaches them through St.Bin.
check(hasMethod(St.Widget, 'add_style_class_name') && hasMethod(St.Widget, 'remove_style_class_name'),
    'the selection highlight uses add_style_class_name/remove_style_class_name');
check(hasMethod(St.Button, 'add_style_class_name'), 'St.Button inherits them from St.Widget');
check(hasMethod(St.Button, 'get_label') && hasMethod(St.Button, 'set_label'),
    'the selected item controls the Pin or Unpin label');

// Scroll view holds one child.
check(hasMethod(St.ScrollView, 'get_child'), 'St.ScrollView holds the row list as its child');
check(hasMethod(St.ScrollView, 'get_vadjustment') && hasMethod(St.Adjustment, 'get_values'),
    'the scroll position and page size define which rows are visible');

// Enums the key handler uses. Digits are not among them: Clutter.Text consumes printable keys
// before the handler runs, so quick select is applied to the text change instead.
check(Clutter.Orientation.VERTICAL !== undefined, 'Clutter.Orientation.VERTICAL exists');
check(Clutter.ActorAlign.START !== undefined, 'Clutter.ActorAlign.START exists');
check(Clutter.ModifierType.SHIFT_MASK !== undefined, 'Clutter.ModifierType.SHIFT_MASK exists');
for (const name of ['KEY_Escape', 'KEY_Up', 'KEY_Down', 'KEY_Home', 'KEY_End', 'KEY_Return', 'KEY_KP_Enter']) {
    check(typeof Clutter[name] === 'number', `Clutter.${name} is a keyval`);
}

// Keybindings: a binding added straight to global.display is filtered out by ShellWM's
// 'filter-keybinding' handler, which only allows names Main.wm put in its allow list. So the
// extension must register through Main.wm with an action mode, and the enums it passes must
// exist.
let Shell, Meta;
try {
    Shell = (await import('gi://Shell')).default;
    Meta = (await import('gi://Meta')).default;
    check(Shell.ActionMode.ALL !== undefined, 'Shell.ActionMode exists (Main.wm.addKeybinding needs a mode)');
    check(Meta.KeyBindingAction.NONE !== undefined, 'Meta.KeyBindingAction exists (what addKeybinding returns)');
    check(Meta.KeyBindingFlags.NONE !== undefined, 'Meta.KeyBindingFlags.NONE exists');
} catch (err) {
    print(`skip: gi://Shell is not importable here (${err.message})`);
}

// The registration itself is pinned at the source level, because Main.wm is shell JS and
// cannot be imported headless.
function readSource(path) {
    const bytes = Gio.File.new_for_path(path).load_contents(null)[1];
    return new TextDecoder().decode(bytes);
}

const extensionSrc = readSource('extension/extension.js');
check(/Main\.wm\.addKeybinding\(\s*'open-popup'/.test(extensionSrc),
    'the keybinding is registered through Main.wm.addKeybinding, not global.display');
check(!/global\.display\.add_keybinding/.test(extensionSrc),
    'extension.js does not call global.display.add_keybinding (that binding is filtered out)');
check(/Main\.wm\.removeKeybinding\('open-popup'\)/.test(extensionSrc),
    'the keybinding is removed through Main.wm, which is what allowed it');
if (Meta) {
    check(Meta.Selection.prototype.set_owner?.length === 2,
        'Mutter set_owner takes both a selection type and a selection source');
}
check(/selection\.set_owner\(Meta\.SelectionType\.SELECTION_CLIPBOARD, source\)/.test(extensionSrc),
    'clipboard restore passes both arguments to Mutter set_owner');
check(/Meta\.SelectionSourceMemory\.new\(offer, GLib\.Bytes\.new\(bytes\)\)/.test(extensionSrc),
    'clipboard restore applies the MIME rule and offers the original bytes');

// The popup wiring is pinned at the source level too: the rules live in popupLogic.js, so the
// UI has to actually consult them rather than reimplement them.
const popupSrc = readSource('extension/popup.js');
check(/popupLogic\.quickSelectFromChange\(this\._lastText, text\)/.test(popupSrc),
    'the quick select rule is applied in the text-changed handler, which is the path a digit ' +
    'actually takes because Clutter.Text consumes printable keys');
check(!/keyval >= KEY\.Digit1/.test(popupSrc),
    'the key handler does not read digits, so it does not claim a path the shell never delivers');
check(/popupLogic\.reachableCount\(this\._pager\)/.test(popupSrc),
    'the selection can reach the row that is not loaded yet, which is what fetches the next page');
check(/this\._nav\.setCount\(query === '' \? popupLogic\.reachableCount\(this\._pager\)/.test(popupSrc),
    'the UI counts the reachable rows, not just the loaded ones');
check(/quickSelectFromChange[\s\S]{0,200}digitIsQuickSelect\(previousText\)/.test(readSource('extension/popupLogic.js')),
    'the quick select rule stays in popupLogic and the change check uses it');
check(/this\._pager\.windowFor\(this\._nav\.index\)/.test(popupSrc),
    'the UI renders the sliding window, not a fixed first page');
check(/const position = this\._nav\.index - this\._windowStart;/.test(popupSrc),
    'the highlight is offset by where the rendered window starts');
check(/if \(this\._resettingText\) return;/.test(popupSrc),
    'the text-changed handler ignores the reset open() performs, so one open is one request');
check(/open\(anchor = null\) \{[\s\S]*?this\._nav = new popupLogic\.Navigator\(0\);[\s\S]*?this\._pager\.ensure\(0\);/.test(popupSrc),
    'each popup open resets the selection before requesting its first page');
check(/this\.close\(\);\s*\n\s*this\._onSelect\(row, richText, pasteTarget\);/.test(popupSrc),
    'selecting closes the popup before restoring the owner, so the modal grab is released for the paste');
check(/_workAreaForPoint\(anchor\?\.x \?\? px, anchor\?\.y \?\? py\)/.test(popupSrc) &&
      /anchor \? anchor\.x - POPUP_WIDTH \/ 2 : px/.test(popupSrc),
    'indicator placement centers over the anchor and keyboard placement still uses the pointer');
check(/if \(\(event\.get_state\(\) & SHORTCUT_MODIFIERS\) !== ALT\) return Clutter\.EVENT_PROPAGATE;/.test(popupSrc),
    'item-action shortcuts leave bare letters and normal Backspace edits to the search entry');
check(/if \(row\.pinned\) \{[\s\S]*?return;[\s\S]*?deleteItem\(row\.id, false, done\)/.test(popupSrc),
    'the popup protects pinned rows and never forces single-item deletion');
check(/if \(!this\._selectedRowIsVisible\(\)\) \{[\s\S]*?return;[\s\S]*?deleteItem\(row\.id, false, done\)/.test(popupSrc),
    'deletion waits until the selected row is inside the scroll viewport');
check(/requestClear\(anchor = null\)[\s\S]*?this\._cancelClearButton\.grab_key_focus\(\)/.test(popupSrc) &&
      /_clearUnpinned\(\) \{\s*if \(!this\._confirmingClear\) return;[\s\S]*?clearHistory\(false,/.test(popupSrc),
    'clear starts with Cancel focused and only clears unpinned history after confirmation');
check(/this\._debouncer\.cancel\(\);\s*this\._query\(this\._entry\.get_text\(\)\);/.test(popupSrc),
    'item-action refresh preserves the current search');
check(/_select\(index, richText = false, pasteTarget = null\) \{\s*if \(this\._confirmingClear \|\| this\._actionPending\) return;/.test(popupSrc) &&
      /this\._clearRows\(\);\s*this\._updateActionButtons\(\);\s*this\._debouncer\.cancel\(\);/.test(popupSrc),
    'an in-progress action blocks old-row selection and clears stale rows before refreshing');
check([...popupSrc.matchAll(/if \(isCurrent\(\)\) this\._onReadError\(\);/g)].length === 2 &&
      /_onReadError\(\) \{[\s\S]*?this\._actionPending = false;/.test(popupSrc),
    'a current history or search read failure unlocks the popup after an action');

if (failures > 0) {
    print(`\n${failures} popup API check(s) failed`);
    system.exit(1);
}
print('\npopup API checks passed');
system.exit(0);

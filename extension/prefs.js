import Adw from 'gi://Adw';
import Gdk from 'gi://Gdk?version=4.0';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import Gtk from 'gi://Gtk?version=4.0';

import {ExtensionPreferences} from 'resource:///org/gnome/Shell/Extensions/js/extensions/prefs.js';
import {DaemonBridge} from './dbus.js';

export default class ClipboardManagerPreferences extends ExtensionPreferences {
    fillPreferencesWindow(window) {
        this._window = window;
        this._settings = this.getSettings();
        this._remoteRows = [];
        this._remoteWidgets = [];
        this._excludedRows = [];
        this._ready = false;
        this._loading = false;
        this._pendingSettings = new Set();
        this._cancellable = new Gio.Cancellable();

        window.set_default_size(700, 530);
        this._buildGeneral();
        this._buildAppearance();
        this._buildPrivacy();
        this._buildStorage();

        this._bridge = new DaemonBridge({
            onConnectionChanged: state => {
                if (state === 'lost') this._setReady(false);
            },
            onSettingsLoaded: (settings, excluded) => this._applySettings(settings, excluded),
        });
        this._setReady(false);
        window.connect('close-request', () => {
            this._window = null;
            this._settings.disconnect(this._shortcutChangedId);
            this._cancellable.cancel();
            this._bridge.destroy();
            return false;
        });
    }

    _page(title, iconName) {
        const page = new Adw.PreferencesPage({title, icon_name: iconName});
        this._window.add(page);
        return page;
    }

    _group(page, title, description) {
        const group = new Adw.PreferencesGroup({title, description});
        page.add(group);
        return group;
    }

    _buildGeneral() {
        const page = this._page('General', 'preferences-system-symbolic');
        const shortcutGroup = this._group(page, 'Shortcut', 'Use GNOME shortcut notation, such as &lt;Super&gt;V.');
        const shortcut = new Adw.EntryRow({title: 'Open Clipboard History', show_apply_button: true});
        const update = () => { shortcut.text = this._settings.get_strv('open-popup')[0] ?? ''; };
        update();
        this._shortcutChangedId = this._settings.connect('changed::open-popup', update);
        shortcut.connect('apply', () => this._saveShortcut(shortcut));
        shortcutGroup.add(shortcut);
        const autoPaste = new Adw.SwitchRow({
            title: 'Paste automatically on Enter or 1–9',
            subtitle: 'Selecting an item also sends Ctrl+V to the original app.',
        });
        shortcutGroup.add(autoPaste);
        this._settings.bind('auto-paste', autoPaste, 'active', Gio.SettingsBindFlags.DEFAULT);
        const directPaste = new Adw.SwitchRow({
            title: 'Direct paste in all apps',
            subtitle: 'Allow automatic paste and Alt+Enter in any app that accepts Ctrl+V.',
        });
        shortcutGroup.add(directPaste);
        this._settings.bind('direct-paste-all-apps', directPaste, 'active', Gio.SettingsBindFlags.DEFAULT);

        const history = this._group(page, 'History', 'Limits apply when new items are saved. Pinned items are kept.');
        this._addRemoteSpin(history, 'Maximum entries', 'max_entries', 1, 100000, 100);
        this._addRemoteSpin(history, 'Retention days', 'retention_days', 1, 3650, 1);
        this._daemonStatus = new Adw.ActionRow({title: 'Connecting to clipboard history…'});
        history.add(this._daemonStatus);
    }

    _saveShortcut(row) {
        const [valid, key, modifiers] = Gtk.accelerator_parse(row.text.trim());
        const globalModifiers = Gdk.ModifierType.SUPER_MASK | Gdk.ModifierType.ALT_MASK |
            Gdk.ModifierType.CONTROL_MASK | Gdk.ModifierType.META_MASK;
        if (!valid || !(modifiers & globalModifiers) || !Gtk.accelerator_valid(key, modifiers)) {
            this._toast('Use a key with Super, Alt, or Ctrl.');
            return;
        }

        const accelerator = Gtk.accelerator_name(key, modifiers);
        const shell = new Gio.Settings({schema_id: 'org.gnome.shell.keybindings'});
        const notificationShortcuts = shell.get_strv('toggle-message-tray');
        if (notificationShortcuts.some(value => {
            const [parsed, trayKey, trayModifiers] = Gtk.accelerator_parse(value);
            return parsed && trayKey === key && trayModifiers === modifiers;
        })) {
            this._toast('GNOME Notifications already uses this shortcut.');
            return;
        }

        const existing = this._settings.get_strv('open-popup');
        if (existing[0] !== accelerator && !this._settings.set_strv('open-popup', [accelerator]))
            this._toast('Could not save the shortcut.');
        else
            row.text = accelerator;
    }

    _buildAppearance() {
        const page = this._page('Appearance', 'preferences-desktop-theme-symbolic');
        const group = this._group(page, 'History Popup', '');
        const compact = new Adw.SwitchRow({title: 'Compact rows'});
        const size = Adw.SpinRow.new_with_range(16, 64, 1);
        size.title = 'Thumbnail display size';
        size.subtitle = 'Image preview size in the popup, in pixels';
        group.add(compact);
        group.add(size);
        this._settings.bind('compact-rows', compact, 'active', Gio.SettingsBindFlags.DEFAULT);
        this._settings.bind('thumbnail-display-size', size, 'value', Gio.SettingsBindFlags.DEFAULT);
    }

    _buildPrivacy() {
        const page = this._page('Privacy', 'preferences-system-privacy-symbolic');
        const group = this._group(page, 'Sensitive Copies', '');
        const filter = new Adw.SwitchRow({
            title: 'Ignore password manager copies',
            subtitle: 'Uses sensitive-content hints from apps. Turning this off can save passwords.',
        });
        this._bindRemote(group, filter, 'filter_passwords', 'active', 'b');
        const entropy = new Adw.SwitchRow({
            title: 'Ignore text that resembles a secret',
            subtitle: 'Entropy filtering may also exclude ordinary random-looking text.',
        });
        this._bindRemote(group, entropy, 'filter_entropy', 'active', 'b');
        const encryption = new Adw.SwitchRow({
            title: 'Encrypt history at rest',
            subtitle: 'Protects saved copies and previews with GNOME Keyring. Image thumbnails are hidden while enabled.',
        });
        this._bindRemote(group, encryption, 'encrypt_at_rest', 'active', 'b',
            'Could not change encryption. Check that your keyring is unlocked.');

        this._exclusions = this._group(page, 'Excluded Applications',
            'Enter the exact application ID, or the window title if the app does not report an ID.');
        const add = new Adw.EntryRow({title: 'Application ID or window title', show_apply_button: true});
        add.connect('apply', () => {
            const id = add.text.trim();
            if (!id) return;
            add.sensitive = false;
            this._bridge.addExcludedApp(id, ok => {
                if (!this._window) return;
                add.sensitive = this._ready;
                if (ok) add.text = '';
                else this._toast('Could not add the application.');
            });
        });
        this._exclusions.add(add);
        this._remoteWidgets.push(this._exclusions);
    }

    _buildStorage() {
        const page = this._page('Storage', 'drive-harddisk-symbolic');
        const cleanup = this._group(page, 'Clear History', '');
        this._addButton(cleanup, 'Clear unpinned history', 'Clear…', () => this._confirmClear(false));
        this._addButton(cleanup, 'Clear all history', 'Clear…', () => this._confirmClear(true));
        const exportGroup = this._group(page, 'Export History', 'Exports may contain sensitive clipboard content.');
        this._addButton(exportGroup, 'JSON with clipboard contents', 'Save…', () => this._export('json'));
        this._addButton(exportGroup, 'CSV with previews and metadata', 'Save…', () => this._export('csv'));
        const importGroup = this._group(page, 'Import History',
            'JSON restores clipboard contents. CSV restores preview text only.');
        this._addButton(importGroup, 'Import JSON backup', 'Open…', () => this._import('json'));
        this._addButton(importGroup, 'Import CSV previews', 'Open…', () => this._import('csv'));
        this._remoteWidgets.push(cleanup, exportGroup, importGroup);
    }

    _addButton(group, title, label, onClick) {
        const row = new Adw.ActionRow({title, use_markup: false});
        const button = new Gtk.Button({label, valign: Gtk.Align.CENTER});
        button.connect('clicked', onClick);
        row.add_suffix(button);
        row.activatable_widget = button;
        group.add(row);
        return row;
    }

    _addRemoteSpin(group, title, key, min, max, step) {
        const spin = Adw.SpinRow.new_with_range(min, max, step);
        spin.title = title;
        this._bindRemote(group, spin, key, 'value', 'i');
    }

    _bindRemote(group, row, key, property, signature, errorMessage = 'Could not save the setting.') {
        this._remoteRows.push({row, key, property});
        group.add(row);
        row.connect(`notify::${property}`, () => {
            if (this._loading || !this._ready || this._pendingSettings.has(key)) return;
            const value = signature === 'i' ? Math.round(row[property]) : row[property];
            this._pendingSettings.add(key);
            row.sensitive = false;
            this._bridge.setSetting(key, new GLib.Variant(signature, value), ok => {
                if (!this._window) return;
                this._pendingSettings.delete(key);
                row.sensitive = this._ready;
                if (!ok) {
                    this._toast(errorMessage);
                }
                this._bridge.refresh();
            });
        });
    }

    _applySettings(settings, excluded) {
        this._loading = true;
        for (const {row, key, property} of this._remoteRows) {
            if (settings[key] !== undefined && !this._pendingSettings.has(key))
                row[property] = settings[key];
        }

        for (const row of this._excludedRows) this._exclusions.remove(row);
        this._excludedRows = [...excluded].sort().map(id => this._addButton(this._exclusions, id, 'Remove', () => {
            this._bridge.removeExcludedApp(id, ok => {
                if (!ok) this._toast('Could not remove the application.');
            });
        }));
        this._loading = false;
        this._setReady(true);
    }

    _setReady(ready) {
        this._ready = ready;
        for (const {row, key} of this._remoteRows)
            row.sensitive = ready && !this._pendingSettings.has(key);
        for (const widget of this._remoteWidgets) widget.sensitive = ready;
        this._daemonStatus.visible = !ready;
        if (!ready && this._bridge && !this._bridge.connected)
            this._daemonStatus.title = 'Clipboard history service unavailable';
    }

    _confirmClear(includePinned) {
        const dialog = new Adw.AlertDialog({
            heading: includePinned ? 'Clear all history?' : 'Clear unpinned history?',
            body: includePinned ? 'Pinned entries will also be deleted.' : 'Pinned entries will be kept.',
        });
        dialog.add_response('cancel', 'Cancel');
        dialog.add_response('clear', 'Clear History');
        dialog.set_response_appearance('clear', Adw.ResponseAppearance.DESTRUCTIVE);
        dialog.default_response = 'cancel';
        dialog.close_response = 'cancel';
        dialog.choose(this._window, this._cancellable, (source, result) => {
            try {
                if (source.choose_finish(result) !== 'clear' || !this._ready || !this._window) return;
                this._bridge.clearHistory(includePinned, count => {
                    this._toast(count === null ? 'Could not finish clearing history.' : `Cleared entries: ${count}`);
                });
            } catch (error) {
                if (this._window) this._toast('Could not open the confirmation.');
            }
        });
    }

    _export(format) {
        const dialog = new Gtk.FileDialog({
            title: 'Export Clipboard History',
            initial_name: `clipboard-history.${format}`,
        });
        dialog.save(this._window, this._cancellable, (source, result) => {
            let path;
            try {
                path = source.save_finish(result).get_path();
            } catch (error) {
                if (!error.matches(Gtk.DialogError, Gtk.DialogError.DISMISSED))
                    this._toast('Could not choose an export file.');
                return;
            }
            if (!path || !this._ready) {
                this._toast('Choose a local file while clipboard history is available.');
                return;
            }
            this._bridge.exportHistory(path, format, count => {
                this._toast(count === null ? 'Could not export history.' : `Exported entries: ${count}`);
            });
        });
    }

    _import(format) {
        const filter = new Gtk.FileFilter({name: format.toUpperCase()});
        filter.add_pattern(`*.${format}`);
        const filters = new Gio.ListStore({item_type: Gtk.FileFilter});
        filters.append(filter);
        const dialog = new Gtk.FileDialog({title: 'Import Clipboard History', filters});
        dialog.open(this._window, this._cancellable, (source, result) => {
            let path;
            try {
                path = source.open_finish(result).get_path();
            } catch (error) {
                if (!error.matches(Gtk.DialogError, Gtk.DialogError.DISMISSED))
                    this._toast('Could not choose an import file.');
                return;
            }
            if (!path || !this._window || !this._ready) return;
            this._bridge.importHistory(path, format, count => {
                this._toast(count === null ? 'Could not import history.' : `Imported entries: ${count}`);
            });
        });
    }

    _toast(title) {
        this._window?.add_toast(new Adw.Toast({title}));
    }
}

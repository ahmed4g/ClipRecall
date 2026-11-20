// Phase 4: the popup. Shell-native (St actors on the ui group), because GNOME Shell 50
// has no layer-shell, so a popup can only be shell UI or a normal window.
//
// The rules the acceptance gate asserts are in popupLogic.js and are checked headlessly;
// this file is the thin shell wiring around them: open at the pointer, key focus through a
// stage grab, keyboard navigation, quick select, and paged rendering.
//
// APIs verified on this machine (GNOME Shell 50.6 / mutter 18), each pinned by
// tests/testPopupApi.js so a rename cannot slip in again:
// - Main.pushModal(actor) returns a Clutter.Grab (global.stage.grab(actor)), and
//   Main.popModal(grab) takes that grab. It is not the old boolean/enum return.
// - Main.layoutManager.uiGroup, addTopChrome, getWorkAreaForMonitor(index).
// - global.get_pointer() returns [x, y].
// - St.Entry, St.Widget, St.Label, St.Button, St.BoxLayout, St.ScrollView exist in St-18.
// - Children are added with add_child(); Clutter.Actor.add_actor() is gone.
// - Keys are read with event.get_key_symbol(); Clutter.Event.get_keyval() is gone.
// - 'text-changed' is a Clutter.Text signal, not an St.Entry signal, so it is connected on
//   the entry's text actor (the same thing the shell's own entry code does).
// - Clutter.Text consumes printable keys before the popup's key handler runs, so a quick
//   select digit arrives as a text change rather than as a key event (measured live on
//   2026-11-06: pressing 1 with an empty entry produced a search for "1").
// - The entry placeholder is the hint-text property; there is no `hint` property.
// - The shell loads stylesheet.css from the extension directory automatically.

import Clutter from 'gi://Clutter';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import St from 'gi://St';
import {ensureActorVisibleInScrollView} from 'resource:///org/gnome/shell/misc/animationUtils.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';

import {Debouncer} from './filters.js';
import * as popupLogic from './popupLogic.js';

const POPUP_WIDTH = 420;
const POPUP_HEIGHT = 480;

const KEY = {
    Escape: Clutter.KEY_Escape,
    Up: Clutter.KEY_Up,
    Down: Clutter.KEY_Down,
    Home: Clutter.KEY_Home,
    End: Clutter.KEY_End,
    Return: Clutter.KEY_Return,
    KP_Enter: Clutter.KEY_KP_Enter,
};

const ALT = Clutter.ModifierType.MOD1_MASK;
const SHORTCUT_MODIFIERS = ALT | Clutter.ModifierType.CONTROL_MASK | Clutter.ModifierType.SHIFT_MASK |
    Clutter.ModifierType.MOD4_MASK | Clutter.ModifierType.SUPER_MASK |
    Clutter.ModifierType.HYPER_MASK | Clutter.ModifierType.META_MASK;

export class Popup {
    constructor(bridge, {settings, onSelect, onTiming} = {}) {
        this._bridge = bridge;
        this._settings = settings;
        this._onSelect = onSelect || (() => {});
        this._onTiming = onTiming || (() => {});

        this._rows = [];
        this._displayRows = [];
        this._rowWidgets = [];
        this._thumbnails = [];
        this._nav = new popupLogic.Navigator(0);
        this._pager = new popupLogic.PageWindow((req) => this._fetchPage(req));
        this._queryText = '';
        // Absolute position of the first rendered row: navigation counts the loaded rows, so
        // the highlight and the quick select badges are offset by this.
        this._windowStart = 0;
        // Requests made through the search path; the pager counts its own.
        this._searchRequests = 0;
        // Entry text before the current change, so a change from empty to a single digit is
        // recognised as a quick select key press.
        this._lastText = '';
        // Set while open() clears the entry, so the reset is not read as the user typing.
        this._resettingText = false;

        this._open = false;
        this._built = false;
        this._grab = null;
        this._openedAt = null;
        this._confirmingClear = false;
        this._actionPending = false;
        this._actionSelection = null;
    }

    _build() {
        if (this._built) return;

        // St.BoxLayout rather than St.Widget: a plain St.Widget has no layout manager, so the
        // entry and the row list would not be arranged inside it.
        this._actor = new St.BoxLayout({
            orientation: Clutter.Orientation.VERTICAL,
            style_class: 'clipboard-manager-popup',
            reactive: true,
            can_focus: true,
            width: POPUP_WIDTH,
            height: POPUP_HEIGHT,
        });

        this._entry = new St.Entry({
            style_class: 'clipboard-manager-search',
            can_focus: true,
        });
        this._entry.set_hint_text('Search history');

        this._list = new St.BoxLayout({
            orientation: Clutter.Orientation.VERTICAL,
            style_class: 'clipboard-manager-list',
        });
        this._scroll = new St.ScrollView({
            style_class: 'clipboard-manager-scroll',
            child: this._list,
            y_expand: true,
        });

        this._actor.add_child(this._entry);
        this._actor.add_child(this._scroll);
        this._buildActions();

        // 'text-changed' belongs to Clutter.Text, not to St.Entry, so it is connected on the
        // entry's text actor.
        this._entry.get_clutter_text().connect('text-changed', () => {
            // open() clears the entry, which fires this signal. Without the guard the reset
            // schedules a query, so every open made two identical page requests: measured live
            // on 2026-11-06 as two render lines for one open.
            if (this._resettingText) return;
            if (this._confirmingClear) return;

            const text = this._entry.get_text();
            // Clutter.Text consumes printable keys before the popup's key handler runs, so a
            // quick select digit arrives here as text rather than as a key event (measured live
            // on 2026-11-06: pressing 1 with an empty entry produced a search for "1"). The rule
            // is applied to the change itself, and the digit does not stay in the entry.
            const digit = popupLogic.quickSelectFromChange(this._lastText, text);
            this._lastText = text;
            if (digit !== null) {
                this._resettingText = true;
                this._entry.set_text('');
                this._lastText = '';
                this._resettingText = false;
                this._select(this._windowStart + digit - 1);
                return;
            }
            this._debouncer.schedule(text);
        });
        this._entry.connect('key-press-event', (actor, event) => this._onKey(event));
        this._actor.connect('key-press-event', (actor, event) => this._onKey(event));
        this._actor.connect('captured-event', (_actor, event) => this._onCapturedEvent(event));
        this._actor.connect('button-press-event', (_actor, event) => this._onButtonPress(event));

        this._debouncer = new Debouncer(
            popupLogic.SEARCH_DEBOUNCE_MS,
            (query) => this._query(query),
            {
                arm: (fn, delayMs) => GLib.timeout_add(GLib.PRIORITY_DEFAULT, delayMs, () => {
                    fn();
                    return GLib.SOURCE_REMOVE;
                }),
                cancel: (id) => GLib.source_remove(id),
            }
        );

        Main.layoutManager.addTopChrome(this._actor);
        this._settings.connectObject(
            'changed::compact-rows', () => this._applyAppearance(),
            'changed::thumbnail-display-size', () => this._applyAppearance(),
            this._actor);
        this._applyAppearance();
        this._built = true;
    }

    _applyAppearance() {
        if (this._settings.get_boolean('compact-rows'))
            this._actor.add_style_class_name('clipboard-manager-compact');
        else
            this._actor.remove_style_class_name('clipboard-manager-compact');

        const size = this._settings.get_int('thumbnail-display-size');
        for (const icon of this._thumbnails) icon.set_icon_size(size);
    }

    _buildActions() {
        this._actions = new St.BoxLayout({style_class: 'clipboard-manager-actions'});
        this._pinButton = this._addAction(this._actions, 'Pin', () => this._togglePin());
        this._deleteButton = this._addAction(this._actions, 'Delete', () => this._deleteSelected());
        this._richTextButton = this._addAction(this._actions, 'Copy Rich Text', () => this._copyRichText());
        this._clearButton = this._addAction(this._actions, 'Clear Unpinned', () => this.requestClear());

        this._confirmation = new St.BoxLayout({style_class: 'clipboard-manager-clear-confirmation'});
        this._confirmation.add_child(new St.Label({text: 'Clear unpinned history?', y_align: Clutter.ActorAlign.CENTER}));
        this._cancelClearButton = this._addAction(this._confirmation, 'Cancel', () => this._cancelClear());
        this._confirmClearButton = this._addAction(this._confirmation, 'Clear', () => this._clearUnpinned());
        this._confirmation.hide();

        this._actionStatus = new St.Label({style_class: 'clipboard-manager-action-status'});
        this._actionStatus.hide();
        this._actor.add_child(this._actions);
        this._actor.add_child(this._confirmation);
        this._actor.add_child(this._actionStatus);
    }

    _addAction(container, label, onClick) {
        const button = new St.Button({
            label,
            style_class: 'button clipboard-manager-action',
            reactive: true,
            can_focus: true,
        });
        button.connect('clicked', onClick);
        container.add_child(button);
        return button;
    }

    requestClear(anchor = null) {
        if (this._actionPending || this._confirmingClear) return;
        this.open(anchor);
        this._confirmingClear = true;
        this._showActionStatus('');
        this._actions.hide();
        this._confirmation.show();
        this._cancelClearButton.grab_key_focus();
    }

    _cancelClear() {
        if (!this._confirmingClear) return;
        this._confirmingClear = false;
        this._confirmation.hide();
        this._actions.show();
        if (this._open) this._entry.grab_key_focus();
    }

    _clearUnpinned() {
        if (!this._confirmingClear) return;
        this._cancelClear();
        this._applyAction(done => this._bridge.clearHistory(false, count => done(Number.isInteger(count))),
            'Could not clear unpinned history.');
    }

    _selectedRow() {
        return this._displayRows[this._nav.index - this._windowStart];
    }

    _selectedWidget() {
        return this._rowWidgets[this._nav.index - this._windowStart];
    }

    _rowIsAllocated(widget) {
        return widget?.has_allocation() && this._list.has_allocation() && this._scroll.has_allocation();
    }

    _revealSelection() {
        const widget = this._selectedWidget();
        if (this._rowIsAllocated(widget)) ensureActorVisibleInScrollView(this._scroll, widget);
    }

    _selectedRowIsVisible() {
        const widget = this._selectedWidget();
        if (!this._rowIsAllocated(widget)) return false;

        const [value, , , , , pageSize] = this._scroll.vadjustment.get_values();
        const {y1, y2} = widget.get_allocation_box();
        const offset = this._list.get_allocation_box().y1;
        return y2 > y1 && y1 + offset >= value && y2 + offset <= value + pageSize;
    }

    _togglePin() {
        const row = this._selectedRow();
        if (!row) return;
        this._applyAction(done => {
            if (row.pinned) this._bridge.unpinItem(row.id, done);
            else this._bridge.pinItem(row.id, done);
        }, 'Could not update the pin.');
    }

    _deleteSelected() {
        if (this._actionPending) return;
        const row = this._selectedRow();
        if (!row) return;
        if (row.pinned) {
            this._showActionStatus('Unpin this item before deleting it.');
            return;
        }
        if (!this._selectedRowIsVisible()) {
            this._revealSelection();
            this._showActionStatus('Wait until the selected item is visible before deleting it.');
            return;
        }
        this._applyAction(done => this._bridge.deleteItem(row.id, false, done), 'Could not delete this item.');
    }

    _applyAction(send, errorMessage) {
        if (!this._open || this._actionPending) return;
        this._actionPending = true;
        this._showActionStatus('');
        this._updateActionButtons();
        this._entry.grab_key_focus();
        send(success => {
            if (!this._open) {
                this._actionPending = false;
                return;
            }
            if (!success) {
                this._actionPending = false;
                this._showActionStatus(errorMessage);
                this._updateActionButtons();
                return;
            }

            // Keep the current search and prefer the same visible row after refresh.
            // After a delete, select the row that took its place, or the last row.
            this._actionSelection = {
                id: this._selectedRow()?.id,
                position: Math.max(0, this._nav.index - this._windowStart),
            };
            this._nav = new popupLogic.Navigator(0);
            this._clearRows();
            this._updateActionButtons();
            this._debouncer.cancel();
            this._query(this._entry.get_text());
        });
    }

    _onReadError() {
        if (!this._actionSelection) return;
        this._actionSelection = null;
        this._actionPending = false;
        this._showActionStatus('Could not refresh history. Search or reopen the popup to retry.');
        this._updateActionButtons();
    }

    _clearRows() {
        this._rowWidgets = [];
        this._displayRows = [];
        this._thumbnails = [];
        this._list.destroy_all_children();
    }

    _showActionStatus(message) {
        this._actionStatus.set_text(message);
        this._actionStatus.visible = message !== '';
    }

    _updateActionButtons() {
        const row = this._selectedRow();
        const pinLabel = row?.pinned ? 'Unpin' : 'Pin';
        if (this._pinButton.get_label() !== pinLabel) this._pinButton.set_label(pinLabel);
        this._setActionEnabled(this._pinButton, !this._actionPending && !!row);
        this._setActionEnabled(this._deleteButton, !this._actionPending && !!row && !row.pinned);
        this._setActionEnabled(this._richTextButton, !this._actionPending && !!row && popupLogic.rowType(row) === 'text');
        this._setActionEnabled(this._clearButton, !this._actionPending);
    }

    _setActionEnabled(button, enabled) {
        if (button.reactive === enabled) return;
        button.set_reactive(enabled);
        button.set_can_focus(enabled);
        if (enabled) button.remove_style_pseudo_class('insensitive');
        else button.add_style_pseudo_class('insensitive');
    }

    open(anchor = null) {
        if (this._open) return;
        const started = GLib.get_monotonic_time();
        this._build();

        const [px, py] = global.get_pointer();
        const work = this._workAreaForPoint(anchor?.x ?? px, anchor?.y ?? py);
        const x = anchor ? anchor.x - POPUP_WIDTH / 2 : px;
        const pos = popupLogic.clampToWorkArea(x, anchor?.y ?? py, POPUP_WIDTH, POPUP_HEIGHT, work);
        this._actor.set_position(pos.x, pos.y);
        this._actor.show();

        this._pasteTarget = global.display.get_focus_window();
        this._grab = Main.pushModal(this._actor);
        this._entry.grab_key_focus();

        this._open = true;
        this._openedAt = started;
        const openedUs = GLib.get_monotonic_time() - started;
        this._onTiming('open', openedUs / 1000, {
            x: pos.x,
            y: pos.y,
            pointer: [px, py],
            anchor: anchor ? [anchor.x, anchor.y] : null,
            work: [work.x, work.y, work.width, work.height],
        });

        // First page on demand; the render happens when the reply lands.
        this._nav = new popupLogic.Navigator(0);
        this._pager = new popupLogic.PageWindow((req) => this._fetchPage(req));
        this._queryText = '';
        this._searchRequests = 0;
        this._lastText = '';
        this._resettingText = true;
        this._entry.set_text('');
        this._lastText = '';
        this._resettingText = false;
        this._showActionStatus('');
        this._updateActionButtons();
        this._pager.ensure(0);
    }

    close() {
        if (!this._open) return;
        this._open = false;
        this._debouncer.cancel();
        if (this._actionSelection) this._actionPending = false;
        this._actionSelection = null;
        const openMs = this._openedAt === null ? 0 : (GLib.get_monotonic_time() - this._openedAt) / 1000;
        if (this._grab) {
            Main.popModal(this._grab);
            this._grab = null;
        }
        this._actor.hide();
        this._cancelClear();
        this._clearRows();
        this._onTiming('close', openMs, {});
    }

    destroy() {
        this.close();
        if (this._actor) this._actor.destroy();
    }

    _workAreaForPoint(x, y) {
        const monitor = Main.layoutManager.monitors.find(
            (m) => x >= m.x && x < m.x + m.width && y >= m.y && y < m.y + m.height)
            || Main.layoutManager.primaryMonitor;
        return Main.layoutManager.getWorkAreaForMonitor(monitor.index);
    }

    _containsPoint(actor, x, y) {
        const [left, top] = actor.get_transformed_position();
        const [width, height] = actor.get_transformed_size();
        return x >= left && x < left + width && y >= top && y < top + height;
    }

    _onCapturedEvent(event) {
        if (event.type() === Clutter.EventType.KEY_PRESS) return this._onActionKey(event);
        if (this._confirmingClear && event.type() === Clutter.EventType.BUTTON_PRESS)
            return this._onButtonPress(event);
        return Clutter.EVENT_PROPAGATE;
    }

    _onButtonPress(event) {
        const [x, y] = event.get_coords();
        if (!this._containsPoint(this._actor, x, y)) {
            this.close();
            return Clutter.EVENT_STOP;
        }
        if (this._confirmingClear && !this._containsPoint(this._confirmation, x, y))
            return Clutter.EVENT_STOP;
        return Clutter.EVENT_PROPAGATE;
    }

    _onActionKey(event) {
        const keyval = event.get_key_symbol();
        if (this._confirmingClear) {
            if (keyval === KEY.Escape) {
                this._cancelClear();
                return Clutter.EVENT_STOP;
            }
            if (keyval === Clutter.KEY_Tab || keyval === Clutter.KEY_ISO_Left_Tab) {
                const focused = global.stage.get_key_focus();
                const next = focused === this._cancelClearButton ? this._confirmClearButton : this._cancelClearButton;
                next.grab_key_focus();
                return Clutter.EVENT_STOP;
            }
            if (keyval === KEY.Return || keyval === KEY.KP_Enter) {
                if (!(event.get_flags() & Clutter.EventFlags.FLAG_REPEATED)) {
                    if (global.stage.get_key_focus() === this._confirmClearButton) this._clearUnpinned();
                    else this._cancelClear();
                }
                return Clutter.EVENT_STOP;
            }
            return Clutter.EVENT_PROPAGATE;
        }
        if ((event.get_state() & SHORTCUT_MODIFIERS) !== ALT) return Clutter.EVENT_PROPAGATE;

        const repeat = (event.get_flags() & Clutter.EventFlags.FLAG_REPEATED) !== 0;
        switch (keyval) {
        case KEY.Return:
        case KEY.KP_Enter:
            if (!repeat) this._select(this._nav.index, false, this._pasteTarget);
            return Clutter.EVENT_STOP;
        case Clutter.KEY_p:
        case Clutter.KEY_P:
            if (!repeat) this._togglePin();
            return Clutter.EVENT_STOP;
        case Clutter.KEY_d:
        case Clutter.KEY_D:
            if (!repeat) this._deleteSelected();
            return Clutter.EVENT_STOP;
        case Clutter.KEY_r:
        case Clutter.KEY_R:
            if (!repeat) this._copyRichText();
            return Clutter.EVENT_STOP;
        case Clutter.KEY_BackSpace:
            if (!repeat) this.requestClear();
            return Clutter.EVENT_STOP;
        default:
            return Clutter.EVENT_PROPAGATE;
        }
    }

    _onKey(event) {
        if (this._confirmingClear) return Clutter.EVENT_PROPAGATE;
        // Clutter.Event.get_keyval() does not exist in mutter 18; the keyval is read with
        // get_key_symbol().
        const keyval = event.get_key_symbol();

        if (keyval === KEY.Escape) {
            this.close();
            return true;
        }
        if (keyval === KEY.Up) {
            this._move(-1);
            return true;
        }
        if (keyval === KEY.Down) {
            this._move(1);
            return true;
        }
        if (keyval === KEY.Home) {
            this._nav.home();
            this._highlight();
            return true;
        }
        if (keyval === KEY.End) {
            this._nav.end();
            this._highlight();
            return true;
        }
        if (keyval === KEY.Return || keyval === KEY.KP_Enter) {
            if (this._nav.index >= 0) this._select(this._nav.index);
            return true;
        }
        // Digits are not handled here: Clutter.Text consumes printable keys before this handler
        // runs, so a quick select digit reaches the popup as text instead of as a key event
        // (measured live on 2026-11-06). The rule is applied in the text-changed handler, which
        // is the path the digit actually takes.
        return false;
    }

    _move(delta) {
        const index = this._nav.move(delta);
        if (index < 0) return;
        // Fetch the next page only when navigation reaches past what is loaded.
        if (this._queryText === '' && this._pager.ensure(index)) return;
        // The rendered window slides with the selection, so crossing a page boundary needs a
        // re-render; inside the window the highlight just moves. Comparing the start rather
        // than the window size matters when the second page is exactly full.
        if (this._queryText === '') {
            this._pager.windowFor(index);   // sets pager.start
            if (this._pager.start !== this._windowStart) {
                this._render(null, '');
                return;
            }
        }
        this._highlight();
    }

    _query(query) {
        const started = GLib.get_monotonic_time();
        this._showActionStatus('');
        this._queryText = query;
        if (query === '') {
            this._pager = new popupLogic.PageWindow((req) => this._fetchPage(req));
            this._pager.ensure(0);
            return;
        }
        const pager = this._pager;
        const request = ++this._searchRequests;
        const isCurrent = () => this._open && this._pager === pager && this._searchRequests === request;
        this._bridge.search(query, popupLogic.MAX_VISIBLE, (tuples) => {
            if (!isCurrent()) return;
            this._rows = tuples.map(popupLogic.rowFromTuple);
            this._render(started, query);
        }, () => {
            if (isCurrent()) this._onReadError();
        });
    }

    _fetchPage(request) {
        const started = GLib.get_monotonic_time();
        const pager = this._pager;
        const isCurrent = () => this._open && this._pager === pager && this._queryText === '';
        this._bridge.getHistory(request.offset, request.limit, 'all', (tuples) => {
            if (!isCurrent()) return;
            pager.append(tuples.map(popupLogic.rowFromTuple));
            this._render(started, '');
        }, () => {
            if (isCurrent()) this._onReadError();
        });
    }

    _render(queryStarted, query) {
        // Navigation counts every loaded row, so the window that contains the selection is
        // what gets rendered; the highlight is offset by where that window starts.
        this._nav.setCount(query === '' ? popupLogic.reachableCount(this._pager) : this._rows.length);
        const rows = query === '' ? this._pager.windowFor(this._nav.index) : this._rows;
        this._windowStart = query === '' ? this._pager.start : 0;

        this._clearRows();
        const widgets = this._rowWidgets;
        const thumbnailSize = this._settings.get_int('thumbnail-display-size');

        for (const section of popupLogic.groupSections(rows)) {
            if (section.rows.length === 0) continue;
            if (section.separator) {
                this._list.add_child(new St.Label({
                    style_class: 'clipboard-manager-separator',
                    text: section.name,
                }));
            }
            for (const row of section.rows) {
                const badge = popupLogic.quickSelectIndex(this._displayRows.length);
                const type = popupLogic.rowType(row);
                const button = new St.Button({
                    style_class: 'clipboard-manager-row',
                    reactive: true,
                    can_focus: false,
                });
                const box = new St.BoxLayout({orientation: Clutter.Orientation.HORIZONTAL});
                box.add_child(new St.Label({
                    style_class: 'clipboard-manager-badge',
                    text: badge === null ? '' : String(badge),
                }));
                box.add_child(new St.Label({
                    style_class: `clipboard-manager-type clipboard-manager-type-${type}`,
                    text: type,
                }));
                if (type === 'image') {
                    const icon = new St.Icon({
                        icon_name: 'image-x-generic-symbolic',
                        icon_size: thumbnailSize,
                        style_class: 'clipboard-manager-thumbnail',
                        y_align: Clutter.ActorAlign.CENTER,
                    });
                    this._thumbnails.push(icon);
                    box.add_child(icon);
                    if (row.hasThumbnail) {
                        this._bridge.getThumbnail(row.id, png => {
                            if (this._rowWidgets === widgets) icon.set_gicon(Gio.BytesIcon.new(png));
                        });
                    }
                }
                box.add_child(new St.Label({
                    style_class: 'clipboard-manager-preview',
                    text: popupLogic.displayPreview(row),
                    x_align: Clutter.ActorAlign.START,
                }));
                button.add_child(box);
                // Position inside the rendered window; a re-render destroys these buttons, so
                // the handler can only fire for the window that is currently displayed.
                const position = this._displayRows.length;
                button.connect('clicked', () => this._select(this._windowStart + position));
                button.connect('notify::allocation', () => {
                    if (button === this._selectedWidget()) this._revealSelection();
                });
                this._list.add_child(button);
                this._displayRows.push(row);
                this._rowWidgets.push(button);
            }
        }

        if (queryStarted !== null && this._actionSelection) {
            const {id, position} = this._actionSelection;
            const sameRow = this._displayRows.findIndex(row => row.id === id);
            const nextPosition = sameRow >= 0 ? sameRow : Math.min(position, this._displayRows.length - 1);
            this._nav.setIndex(this._windowStart + nextPosition);
            this._actionSelection = null;
            this._actionPending = false;
        }
        this._highlight();

        if (queryStarted !== null) {
            // start and requests are what make the paging rule measurable live: a window that
            // starts past the first page can only exist if a second page was fetched.
            this._onTiming('render', (GLib.get_monotonic_time() - queryStarted) / 1000, {
                query,
                rows: this._displayRows.length,
                start: this._windowStart,
                requests: query === '' ? this._pager.requests : this._searchRequests,
            });
        }
    }

    _highlight() {
        // The selection index is absolute across the loaded rows; the widgets are the window.
        const position = this._nav.index - this._windowStart;
        this._rowWidgets.forEach((widget, i) => {
            if (i === position) {
                widget.add_style_class_name('clipboard-manager-selected');
            } else {
                widget.remove_style_class_name('clipboard-manager-selected');
            }
        });
        this._revealSelection();
        this._updateActionButtons();
    }

    _copyRichText() {
        const row = this._selectedRow();
        if (row && popupLogic.rowType(row) === 'text') this._select(this._nav.index, true);
    }

    _select(index, richText = false, pasteTarget = null) {
        if (this._confirmingClear || this._actionPending) return;
        const row = this._displayRows[index - this._windowStart];
        if (!row) return;
        this._nav.setIndex(index);
        this._highlight();
        // The row id is logged, never the preview, so the live gate can match the key press
        // to the row the daemon returned without putting clipboard content in the journal.
        this._onTiming('select', 0, {id: row.id, index});
        // Selecting closes the popup before the payload is restored. The user pastes with
        // Ctrl+V into the window that gets focus back, and while the popup holds the modal
        // grab a clipboard read from another client does not complete (measured 2026-11-06:
        // wl-paste and wl-copy hung until the popup closed), so the grab has to be released
        // first.
        this.close();
        this._onSelect(row, richText, pasteTarget);
    }

    get isOpen() {
        return this._open;
    }
}

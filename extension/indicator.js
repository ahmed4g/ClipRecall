import Clutter from 'gi://Clutter';
import GObject from 'gi://GObject';
import St from 'gi://St';
import * as PanelMenu from 'resource:///org/gnome/shell/ui/panelMenu.js';
import * as PopupMenu from 'resource:///org/gnome/shell/ui/popupMenu.js';

export const Indicator = GObject.registerClass({
    GTypeName: 'ClipboardManagerPanelIndicator',
}, class Indicator extends PanelMenu.Button {
    _init({onOpen, onPause, onClear, onPreferences}) {
        super._init(0.5, 'Clipboard Manager');
        this.add_style_class_name('clipboard-manager-indicator');

        this._icon = new St.Icon({
            style_class: 'system-status-icon clipboard-manager-indicator-icon',
        });
        this.add_child(this._icon);

        const openHistory = () => this._showAtAnchor(onOpen);
        const togglePaused = () => {
            if (!this._available) return;
            this.menu.close();
            onPause(!this._paused);
        };

        this._statusItem = new PopupMenu.PopupMenuItem('', {reactive: false, can_focus: false});
        this.menu.addMenuItem(this._statusItem);
        this.menu.addAction('Open History', openHistory);
        this._pauseItem = this.menu.addAction('', togglePaused);
        this._clearItem = this.menu.addAction('Clear Unpinned History…', () => {
            if (this._available) this._showAtAnchor(onClear);
        });
        if (onPreferences) {
            this.menu.addAction('Preferences…', () => {
                this.menu.close();
                onPreferences();
            });
        }
        this._paused = false;
        this.setAvailable(false);

        // Shell 50's inherited click gesture toggles the menu on press. Replace its
        // pointer handling while keeping the native keyboard menu and navigation.
        this._clickGesture.set_enabled(false);
        this.connect('button-press-event', (_actor, event) => {
            switch (event.get_button()) {
            case Clutter.BUTTON_PRIMARY:
                openHistory();
                break;
            case Clutter.BUTTON_MIDDLE:
                togglePaused();
                break;
            case Clutter.BUTTON_SECONDARY:
                this.menu.toggle();
                break;
            default:
                return Clutter.EVENT_PROPAGATE;
            }
            return Clutter.EVENT_STOP;
        });
    }

    _showAtAnchor(callback) {
        this.menu.close();
        const [x, y] = this._icon.get_transformed_position();
        const [width, height] = this._icon.get_transformed_size();
        callback({x: Math.round(x + width / 2), y: Math.round(y + height)});
    }

    setPaused(paused) {
        this._paused = paused;
        this._updateStatus();
    }

    setAvailable(available) {
        this._available = available;
        this._pauseItem.setSensitive(available);
        this._clearItem.setSensitive(available);
        this._updateStatus();
    }

    _updateStatus() {
        const paused = this._paused;
        let status = 'Clipboard daemon unavailable';
        if (this._available)
            status = paused ? 'Monitoring paused' : 'Monitoring active';
        this.accessible_name = `Clipboard Manager, ${status.toLowerCase()}`;
        this._statusItem.label.text = status;
        this._pauseItem.label.text = paused ? 'Resume Monitoring' : 'Pause Monitoring';
        this._icon.icon_name = paused ? 'media-playback-pause-symbolic' : 'edit-paste-symbolic';
        if (paused)
            this.add_style_pseudo_class('paused');
        else
            this.remove_style_pseudo_class('paused');
    }
});

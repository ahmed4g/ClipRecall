# Packaging & Deployment

## RPM (System Installation on Fedora 44)

```
packaging/clipboard-manager.spec
  %files
    /usr/bin/clipboard-manager-daemon
    /usr/share/dbus-1/services/org.clipboardmanager.Daemon.service
    /usr/lib/systemd/user/clipboard-manager-daemon.service
    /usr/share/gnome-shell/extensions/clipboard-manager@project.local/
    /usr/share/gnome-shell/extensions/clipboard-manager@project.local/schemas/gschemas.compiled
```

On Fedora 44 with `rpm-build` and the spec's build requirements installed, build from the
repository root:

```bash
rpm_root="$PWD/build/rpm"
mkdir -p "$rpm_root/SOURCES"
tar --transform='s|^|clipboard-manager-0.1.0/|' \
    --exclude='gschemas.compiled' --exclude='__pycache__' \
    -czf "$rpm_root/SOURCES/clipboard-manager-0.1.0.tar.gz" \
    Cargo.toml Cargo.lock LICENSE README.md daemon data docs extension packaging tests
rpmbuild -ba --define "_topdir $rpm_root" packaging/clipboard-manager.spec
```

Cargo uses the pinned lockfile. Set `CARGO_NET_OFFLINE=true` when its dependency cache is populated.

### D-Bus Activation Unit (`org.clipboardmanager.Daemon.service`)

```ini
[D-BUS Service]
Name=org.clipboardmanager.Daemon
Exec=/usr/bin/clipboard-manager-daemon
SystemdService=clipboard-manager-daemon.service
```

### Systemd User Service (`clipboard-manager-daemon.service`)

```ini
[Unit]
Description=ClipRecall clipboard history daemon
PartOf=graphical-session.target
After=graphical-session.target

[Service]
Type=dbus
BusName=org.clipboardmanager.Daemon
ExecStart=/usr/bin/clipboard-manager-daemon
Restart=on-failure
RestartSec=2
Slice=app.slice

[Install]
WantedBy=default.target
```

The extension never manages daemon processes. Calling the session bus activates the daemon
automatically through systemd, ensuring the daemon can be upgraded or restarted without restarting GNOME Shell.

---

## Local User Installation (`make install-user`)

For development and fast local iteration on Fedora without root permissions:

```bash
make install-user
```

Installs:
- Binary: `~/.local/bin/clipboard-manager-daemon`
- D-Bus Service: `~/.local/share/dbus-1/services/org.clipboardmanager.Daemon.service`
- Systemd Unit: `$XDG_CONFIG_HOME/systemd/user/clipboard-manager-daemon.service`, with `~/.config` as the default configuration directory.
- Extension: `~/.local/share/gnome-shell/extensions/clipboard-manager@project.local/`
- GSettings: compiled schemas into extension directory.
- Enables the extension if GNOME has registered it. For a fresh UUID, log back in and run
  `gnome-extensions enable clipboard-manager@project.local`.
- Reloads user systemd: `systemctl --user daemon-reload && systemctl --user restart clipboard-manager-daemon`

`PREFIX` controls the binary location. The installer rewrites both service files to use that
binary. `make install-user-files` stages just the binary and service files. Set
`USER_CONFIG_DIR` and `USER_DATA_DIR` to temporary paths to test the generated files in isolation.

---

## Flatpak Limitations

GNOME Shell extensions run inside the compositor process and **cannot be sandboxed or installed via Flatpak**.
Therefore, RPM and local user installation are the supported deployment paths.

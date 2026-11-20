# ClipRecall

ClipRecall is a keyboard-first, Maccy-inspired clipboard history app for GNOME on
Wayland. Press **Super+V** to search recent copies and restore text, images, or file references.
Pin entries you use often, pause capture from the panel menu, or exclude specific applications
in Preferences.

The GNOME Shell extension handles clipboard capture, the popup, and shortcuts. A Rust daemon
stores history in SQLite and serves it over the session D-Bus. The extension declares support
for GNOME Shell 50, which was tested on Fedora 44. Fedora 45 and GNOME Shell 51 are unverified.

## Build from source

Run these commands on Fedora 44 from the repository root. The locked dependencies require Rust
1.89 or newer. Cargo downloads dependencies on the first build unless they are already cached.

```bash
sudo dnf install rust cargo gcc make glib2-devel gjs zip
make build
make pack
```

- `make build` compiles the Rust daemon to `target/release/clipboard-manager-daemon`.
- `make pack` compiles the GSettings schema and bundles the JavaScript extension into
  `build/clipboard-manager@project.local.shell-extension.zip`.
- `make test` runs the Rust and GJS checks. The Shell API checks use the GNOME and Mutter
  typelibs when available.

## Install and start

From a GNOME Wayland session, install the build for your user account:

```bash
make install-user
```

Log out and back in so GNOME loads the installed extension. If the installer asks you to enable
a newly installed extension after login, run:

```bash
gnome-extensions enable clipboard-manager@project.local
```

For a system-wide Fedora RPM build, see [Packaging and deployment](docs/packaging.md).

## Use ClipRecall

See the [user guide](docs/user-guide.md) for everyday workflows, privacy settings, and troubleshooting.

| Shortcut | Action |
| --- | --- |
| **Super+V** | Open clipboard history. |
| **↑ / ↓** | Move the highlight. Type in the search box to filter history. |
| **Enter** | Restore the highlighted item to the clipboard. Paste it with your app’s Paste command. |
| **1–9** | Restore the entry with that number when the search box is empty. |
| **Alt+Enter** | Paste directly into the original window if it still has focus. |
| **Alt+R** | Restore the highlighted item’s saved rich text, when available. |

Direct paste is enabled for GNOME Text Editor, Firefox, and Google Chrome. To send **Ctrl+V**
to other apps too, turn on **Preferences → General → Direct paste in all apps**. Apps with a
different paste shortcut can use their own Paste command.

Open **Preferences…** from the panel menu or run
`gnome-extensions prefs clipboard-manager@project.local`. Preferences include history limits,
application exclusions, password-manager filtering, export and clear controls, and optional
encryption at rest using GNOME Keyring. See [Security and privacy](docs/security.md).

## Project layout

- [`extension/`](extension/) contains the GNOME Shell extension and libadwaita preferences.
- [`daemon/`](daemon/) contains the Rust service, history storage, search, and encryption.
- [`packaging/`](packaging/) contains the RPM spec and service files.
- [`plan.md`](plan.md) tracks milestones and acceptance criteria.

## License

ClipRecall is distributed under the [MIT license](LICENSE). Third-party dependencies retain their own licenses.

## Acknowledgments

ClipRecall was developed by Ahmed with assistance from OpenAI Codex for implementation,
debugging, testing, and documentation. Its clipboard history workflow was inspired by Maccy.

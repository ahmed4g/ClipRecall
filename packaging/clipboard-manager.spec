%global uuid clipboard-manager@project.local
%{!?_userunitdir:%global _userunitdir %{_prefix}/lib/systemd/user}

Name:           clipboard-manager
Version:        0.1.0
Release:        1%{?dist}
Summary:        ClipRecall clipboard history for GNOME Wayland

License:        MIT
Source0:        %{name}-%{version}.tar.gz

BuildRequires:  rust >= 1.89
BuildRequires:  cargo
BuildRequires:  gcc
BuildRequires:  glib2-devel
# Runs the extension's headless checks in %check. gjs is also what the extension
# needs at runtime, so it is a hard requirement rather than a build-time convenience.
BuildRequires:  gjs

Requires:       gnome-shell >= 50
Requires:       gjs >= 1.80

%description
A lightweight, fast, keyboard-first clipboard manager built natively for GNOME on Wayland,
inspired by Maccy. This package installs the Rust daemon, the GNOME Shell extension, its
GSettings schema, the systemd user unit and the D-Bus activation file.

%prep
%autosetup

%build
cargo build --release --locked

%install
install -D -p -m 0755 "${CARGO_TARGET_DIR:-target}/release/clipboard-manager-daemon" %{buildroot}%{_bindir}/clipboard-manager-daemon

install -D -p -m 0644 packaging/clipboard-manager-daemon.service %{buildroot}%{_userunitdir}/clipboard-manager-daemon.service
install -D -p -m 0644 packaging/org.clipboardmanager.Daemon.service %{buildroot}%{_datadir}/dbus-1/services/org.clipboardmanager.Daemon.service

# The extension. GNOME Shell reads the schema from the extension's own schemas/ directory,
# so the compiled form has to ship with it; glib-compile-schemas is run at build time.
glib-compile-schemas extension/schemas/
for f in metadata.json extension.js filters.js dbus.js popup.js popupLogic.js indicator.js prefs.js stylesheet.css; do
  install -D -p -m 0644 "extension/$f" %{buildroot}%{_datadir}/gnome-shell/extensions/%{uuid}/$(basename "$f")
done
install -D -p -m 0644 extension/schemas/org.clipboardmanager.gschema.xml %{buildroot}%{_datadir}/gnome-shell/extensions/%{uuid}/schemas/org.clipboardmanager.gschema.xml
install -D -p -m 0644 extension/schemas/gschemas.compiled %{buildroot}%{_datadir}/gnome-shell/extensions/%{uuid}/schemas/gschemas.compiled

%check
# The daemon side.
cargo test --release --locked

# The extension's pure logic, parsed and executed with the engine the shell actually uses.
# tests/testPopupApi.js and tests/testReadPath.js are not run here: they need the versioned
# mutter and gnome-shell typelibs, which are not present in a build root. They run on a real
# session (make test-extension).
gjs -m tests/testSyntax.js
gjs -m tests/testFilters.js
gjs -m tests/testDbusSignatures.js
gjs -m tests/testPopupLogic.js

%files
%license LICENSE
%{_bindir}/clipboard-manager-daemon
%{_userunitdir}/clipboard-manager-daemon.service
%{_datadir}/dbus-1/services/org.clipboardmanager.Daemon.service
%{_datadir}/gnome-shell/extensions/%{uuid}/

%changelog
* Fri Nov 06 2026 Ahmed <ahmed@local> - 0.1.0-1
- Package the extension files and the compiled GSettings schema, which the earlier spec
  installed nothing of, and add a %check section that runs the daemon tests and the
  extension's headless checks.

* Thu Nov 05 2026 Ahmed <ahmed@local> - 0.1.0-1
- Initial Phase 1 release

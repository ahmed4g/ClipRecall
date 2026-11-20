UUID  ?= clipboard-manager@project.local
PREFIX ?= $(HOME)/.local
USER_CONFIG_DIR ?= $(or $(XDG_CONFIG_HOME),$(HOME)/.config)
USER_DATA_DIR ?= $(or $(XDG_DATA_HOME),$(HOME)/.local/share)
CARGO  ?= cargo
# The Meta typelib lives in a versioned mutter directory, not in the default search path.
GI_TYPELIB_PATH ?= $(firstword $(wildcard /usr/lib64/mutter-* /usr/lib/mutter-*))
# St-18 lives in the gnome-shell directory and its typelib references libst-18.so, which is
# not on the default library path, so the popup API check sets both.
SHELL_DIR ?= $(firstword $(wildcard /usr/lib64/gnome-shell /usr/lib/gnome-shell))

.PHONY: all build test test-rust test-extension test-extension-live schemas pack install-user-files install-user clean

all: build schemas

build:
	$(CARGO) build --release --locked

test: test-rust test-extension

test-rust:
	$(CARGO) test

# gjs treats a plain .js file as a script, so the module tests must be run with -m.
# The extension files are parsed first: a syntax error in extension.js only shows up when
# GNOME Shell loads it, which is far too late (it happened on 2026-11-05). The check uses
# gjs itself, because V8 (node --check) accepted the file SpiderMonkey rejected on
# 2026-11-06 — the engine the shell actually uses is the one that matters.
test-extension:
	gjs -m tests/testSyntax.js
	gjs -m tests/testFilters.js
	gjs -m tests/testDbusSignatures.js
	gjs -m tests/testPopupLogic.js
	LD_LIBRARY_PATH=$(SHELL_DIR):$(GI_TYPELIB_PATH) GI_TYPELIB_PATH=$(SHELL_DIR):$(GI_TYPELIB_PATH) gjs -m tests/testPopupApi.js
	GI_TYPELIB_PATH=$(GI_TYPELIB_PATH) gjs -m tests/testReadPath.js

# Live half: needs the daemon running on the session bus. Skips (exit 0) when the
# bus name has no owner, so it is not part of `test`.
test-extension-live:
	gjs -m tests/testBridgeLive.js

schemas:
	glib-compile-schemas extension/schemas/

pack: schemas
	mkdir -p build
	cd extension && zip -9 -r -FS ../build/$(UUID).shell-extension.zip \
		metadata.json extension.js filters.js dbus.js popup.js popupLogic.js indicator.js prefs.js stylesheet.css schemas

# Installs to ~/.local and enables the extension. A logout/login is required for
# GNOME Shell to load a newly installed UUID.
install-user-files: build
	install -D -m 0755 "$(or $(CARGO_TARGET_DIR),target)/release/clipboard-manager-daemon" "$(PREFIX)/bin/clipboard-manager-daemon"
	mkdir -p "$(USER_CONFIG_DIR)/systemd/user" "$(USER_DATA_DIR)/dbus-1/services"
	sed 's|=/usr/bin/clipboard-manager-daemon$$|="$(abspath $(PREFIX))/bin/clipboard-manager-daemon"|' \
		packaging/clipboard-manager-daemon.service > "$(USER_CONFIG_DIR)/systemd/user/clipboard-manager-daemon.service"
	sed 's|=/usr/bin/clipboard-manager-daemon$$|="$(abspath $(PREFIX))/bin/clipboard-manager-daemon"|' \
		packaging/org.clipboardmanager.Daemon.service > "$(USER_DATA_DIR)/dbus-1/services/org.clipboardmanager.Daemon.service"

install-user: install-user-files pack
	gnome-extensions install --force build/$(UUID).shell-extension.zip
	systemctl --user daemon-reload
	systemctl --user enable clipboard-manager-daemon.service
	systemctl --user restart clipboard-manager-daemon.service
	@if gnome-extensions info "$(UUID)" >/dev/null 2>&1; then \
		gnome-extensions enable "$(UUID)"; \
		printf '%s\n' 'Installed. Log out and back in to load the extension files.'; \
	else \
		printf '%s\n' "Installed. Log out and back in, then run: gnome-extensions enable $(UUID)"; \
	fi

clean:
	$(CARGO) clean
	rm -rf build

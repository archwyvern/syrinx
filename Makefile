# syrinx build. `make` produces dist/ with everything a consumer needs:
#
#   dist/bin/syrinx            the CLI
#   dist/bin/syrinx-player     the player
#   dist/lib/libsyrinx.so      the shared library (syrinx.dll on Windows)
#   dist/include/syrinx.h      C header
#   dist/prelude.js             the prelude, for editors and tooling
#   dist/math.js                the standard math (evaluate before anything else)
#   dist/run.js                 the run wrapper
#   dist/framework/             the framework, to copy into a project
#
# V8 is linked statically into both binaries; the first build downloads the prebuilt V8 for the
# host target (~100 MB, needs network), later builds do not.

CARGO ?= cargo
PROFILE ?= release
TARGET_DIR := target/$(PROFILE)
DIST := dist

ifeq ($(OS),Windows_NT)
  LIB := syrinx.dll
  BIN := syrinx.exe
  PLAYER := syrinx-player.exe
else
  UNAME := $(shell uname -s)
  ifeq ($(UNAME),Darwin)
    LIB := libsyrinx.dylib
  else
    LIB := libsyrinx.so
  endif
  BIN := syrinx
  PLAYER := syrinx-player
endif

.PHONY: all build dist install uninstall install-user uninstall-user test check clean examples docs

all: dist

build:
	$(CARGO) build --$(PROFILE)

dist: build
	rm -rf $(DIST)
	mkdir -p $(DIST)/bin $(DIST)/lib $(DIST)/include
	cp $(TARGET_DIR)/$(BIN) $(DIST)/bin/
	cp $(TARGET_DIR)/$(PLAYER) $(DIST)/bin/
	cp $(TARGET_DIR)/$(LIB) $(DIST)/lib/
	cp include/syrinx.h $(DIST)/include/
	cp prelude/prelude.js $(DIST)/prelude.js
	cp prelude/math.js $(DIST)/math.js
	cp prelude/run.js $(DIST)/run.js
	cp prelude/syrinx.d.ts $(DIST)/syrinx.d.ts
	cp -r framework $(DIST)/framework
	@echo; ls -la $(DIST)/bin $(DIST)/lib

# Puts the CLI and the player on PATH (cargo's own bin directory). The shared library, header
# and C# binding are consumed from dist/ or through native-bin, so nothing is installed
# system-wide.
install:
	$(CARGO) install --path crates/syrinx-cli --locked
	$(CARGO) install --path crates/syrinx-player --locked
	@echo; command -v syrinx && syrinx info

uninstall:
	$(CARGO) uninstall syrinx-cli
	$(CARGO) uninstall syrinx-player

# Linux desktop integration for the player, no sudo: a launcher entry, the icon, the shared
# `audio/x-syrinx` MIME type (the same file the VLC plugin installs, so the two never diverge)
# and the player as the default handler. Double-clicking a .syr in the file manager then plays it.
APPS := $(HOME)/.local/share/applications
install-user: install
	mkdir -p $(APPS) $(HOME)/.local/share/icons/hicolor/512x512/apps $(HOME)/.local/share/mime/packages
	sed 's|^Exec=syrinx-player|Exec=$(HOME)/.cargo/bin/syrinx-player|' desktop/syrinx-player.desktop > $(APPS)/syrinx-player.desktop
	cp logo/syrinx-512.png $(HOME)/.local/share/icons/hicolor/512x512/apps/syrinx-player.png
	cp vlc/syrinx-mime.xml $(HOME)/.local/share/mime/packages/syrinx-mime.xml
	update-mime-database $(HOME)/.local/share/mime
	update-desktop-database $(APPS)
	xdg-mime default syrinx-player.desktop audio/x-syrinx
	@echo; echo "audio/x-syrinx ->" $$(xdg-mime query default audio/x-syrinx)

uninstall-user:
	rm -f $(APPS)/syrinx-player.desktop $(HOME)/.local/share/icons/hicolor/512x512/apps/syrinx-player.png
	-update-desktop-database $(APPS)

# The Rust tests, then the JS host's. The JS half needs the CLI, because its tests COMPARE against
# it — the determinism check's diagnostics and every example's samples — rather than asserting what
# this repository believes about itself. They fail rather than skip when it is missing, so `build`
# is a prerequisite and not a convenience.
test: build
	$(CARGO) test --$(PROFILE)
	node --test test/*.test.js

check:
	$(CARGO) clippy --$(PROFILE) --all-targets -- -D warnings
	$(CARGO) fmt --check

# The generated API reference: the JSON a documentation site renders, and docs/API.md from it. Both are
# committed; crates/syrinx-core/tests/docs.rs and test/docs.test.js fail when either is behind.
docs: build
	$(TARGET_DIR)/$(BIN) docs --out docs/syrinx-docs.json
	node tools/api-md.mjs docs/syrinx-docs.json docs/API.md

# Render every example into examples/out/ as a smoke test.
examples: build
	mkdir -p examples/out
	for f in examples/*.syr; do $(TARGET_DIR)/$(BIN) compile $$f -o examples/out/$$(basename $$f .syr).wav || exit 1; done

clean:
	$(CARGO) clean
	rm -rf $(DIST) examples/out

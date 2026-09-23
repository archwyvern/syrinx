# Builds libsyrinx_plugin.so, a VLC 3 demux module that plays .syr files.
#
#   make                       # needs vlc-devel (pkg-config vlc-plugin) and ../dist (run `make` at the root first)
#   VLC_PLUGIN_PATH=$PWD/build vlc ../examples/laser.syr
#   sudo make install          # copies the plugin into VLC's demux plugin dir + libsyrinx.so beside it
#   make install-user          # no sudo: ~/.local/lib/vlc/plugins + VLC_PLUGIN_PATH via environment.d
#
# VLC 3 scans only its own plugin dir and the directories in VLC_PLUGIN_PATH; there is no
# per-user directory it looks at on its own, which is what install-user works around.
#
# The plugin links libsyrinx.so with an rpath of $ORIGIN, so it looks for the library next to
# itself first, then on the normal library path. Rebuild with `make` after `vlc --reset-plugins-cache`
# if VLC keeps ignoring a freshly installed plugin.

CC ?= cc
PKG_CONFIG ?= pkg-config
VLC_CFLAGS ?= $(shell $(PKG_CONFIG) --cflags vlc-plugin)
VLC_LIBS ?= $(shell $(PKG_CONFIG) --libs vlc-plugin)
PLUGINSDIR ?= $(shell $(PKG_CONFIG) --variable=pluginsdir vlc-plugin)

DIST := $(abspath ../dist)
BUILD := build
PLUGIN := $(BUILD)/libsyrinx_plugin.so

all: $(PLUGIN)

$(PLUGIN): syrinx_vlc.c $(DIST)/include/syrinx.h $(DIST)/lib/libsyrinx.so
	mkdir -p $(BUILD)
	$(CC) -std=gnu11 -O2 -Wall -Wextra -fPIC -shared \
		-DMODULE_STRING=\"syrinx\" $(VLC_CFLAGS) -I$(DIST)/include \
		syrinx_vlc.c -o $@ \
		-L$(DIST)/lib -lsyrinx $(VLC_LIBS) -Wl,-rpath,'$$ORIGIN' -Wl,-rpath,$(DIST)/lib
	cp $(DIST)/lib/libsyrinx.so $(BUILD)/

install: $(PLUGIN)
	install -D -m 755 $(PLUGIN) $(PLUGINSDIR)/demux/libsyrinx_plugin.so
	install -D -m 755 $(DIST)/lib/libsyrinx.so $(PLUGINSDIR)/demux/libsyrinx.so
	-vlc-cache-gen $(PLUGINSDIR) 2>/dev/null || true

uninstall:
	rm -f $(PLUGINSDIR)/demux/libsyrinx_plugin.so $(PLUGINSDIR)/demux/libsyrinx.so

USER_PLUGINSDIR := $(HOME)/.local/lib/vlc/plugins
ENV_CONF := $(HOME)/.config/environment.d/50-vlc-syrinx.conf

install-user: $(PLUGIN)
	install -D -m 755 $(PLUGIN) $(USER_PLUGINSDIR)/demux/libsyrinx_plugin.so
	install -D -m 755 $(DIST)/lib/libsyrinx.so $(USER_PLUGINSDIR)/demux/libsyrinx.so
	rm -f $(USER_PLUGINSDIR)/plugins.dat
	@if [ ! -f $(ENV_CONF) ]; then \
		mkdir -p $(dir $(ENV_CONF)); \
		printf 'VLC_PLUGIN_PATH=%s\n' '$(USER_PLUGINSDIR)' > $(ENV_CONF); \
		echo "wrote $(ENV_CONF) (applies to desktop-launched VLC after next login)"; \
	fi
	@echo "for this shell:  export VLC_PLUGIN_PATH=$(USER_PLUGINSDIR)"
	@# Make it visible to the running desktop session too, so VLC launched from the file
	@# manager sees it before the next login.
	-VLC_PLUGIN_PATH=$(USER_PLUGINSDIR) systemctl --user import-environment VLC_PLUGIN_PATH 2>/dev/null
	-dbus-update-activation-environment --systemd VLC_PLUGIN_PATH=$(USER_PLUGINSDIR) 2>/dev/null
	@# Shadow the system vlc.desktop with one that carries the variable in Exec, so VLC
	@# started from any launcher (Files, browser downloads, the app grid) finds the plugin
	@# regardless of that launcher's environment. No re-login needed.
	mkdir -p $(HOME)/.local/share/applications
	sed -e 's|^Exec=|Exec=env VLC_PLUGIN_PATH=$(USER_PLUGINSDIR) |' \
	    -e 's|^MimeType=|MimeType=audio/x-syrinx;|' /usr/share/applications/vlc.desktop \
		> $(HOME)/.local/share/applications/vlc.desktop
	-update-desktop-database $(HOME)/.local/share/applications 2>/dev/null
	@# Teach the desktop what .syr is and hand it to VLC.
	install -D -m 644 syrinx-mime.xml $(HOME)/.local/share/mime/packages/syrinx.xml
	update-mime-database $(HOME)/.local/share/mime
	xdg-mime default vlc.desktop audio/x-syrinx

uninstall-user:
	rm -rf $(USER_PLUGINSDIR)/demux/libsyrinx_plugin.so $(USER_PLUGINSDIR)/demux/libsyrinx.so $(ENV_CONF)
	rm -f $(HOME)/.local/share/mime/packages/syrinx.xml $(HOME)/.local/share/applications/vlc.desktop
	-update-mime-database $(HOME)/.local/share/mime
	-update-desktop-database $(HOME)/.local/share/applications 2>/dev/null

clean:
	rm -rf $(BUILD)

.PHONY: all install uninstall install-user uninstall-user clean

# Builds libgstsyrinx.so, a GStreamer plugin that plays .syr files in any GStreamer player
# (GNOME's Decibels and Showtime, gst-play, playbin).
#
#   make                       # needs the GStreamer headers (see `make headers`) and ../dist
#   make headers               # no sudo: extracts gstreamer1-devel + -plugins-base-devel into build/headers
#   GST_PLUGIN_PATH=$PWD/build gst-play-1.0 ../examples/laser.syr
#   make install-user          # ~/.local/share/gstreamer-1.0/plugins, which GStreamer scans on its own
#
# GStreamer looks in ~/.local/share/gstreamer-1.0/plugins without any environment variable, so
# unlike the VLC module there is nothing to inject into the session. The plugin links libsyrinx.so
# with an rpath of $ORIGIN and the library is installed beside it.
#
# The headers: when gstreamer1-devel is installed, pkg-config finds them. When it is not (and
# installing needs sudo), `make headers` downloads the two rpms with dnf and unpacks them under
# build/headers; the libraries themselves are the ones already on the system, linked by soname.

CC ?= cc
PKG_CONFIG ?= pkg-config

DIST := $(abspath ../dist)
BUILD := build
PLUGIN := $(BUILD)/libgstsyrinx.so
VERSION := $(shell sed -n 's/^version = "\(.*\)"/\1/p' ../Cargo.toml | head -1)

HAVE_GST_DEVEL := $(shell $(PKG_CONFIG) --exists gstreamer-audio-1.0 && echo yes)
ifeq ($(HAVE_GST_DEVEL),yes)
  GST_CFLAGS := $(shell $(PKG_CONFIG) --cflags gstreamer-1.0 gstreamer-base-1.0 gstreamer-audio-1.0)
  GST_LIBS := $(shell $(PKG_CONFIG) --libs gstreamer-1.0 gstreamer-base-1.0 gstreamer-audio-1.0)
else
  HEADERS := $(BUILD)/headers/usr/include/gstreamer-1.0
  GST_CFLAGS := -I$(HEADERS) $(shell $(PKG_CONFIG) --cflags glib-2.0 gobject-2.0)
  # Link by soname against the installed runtime libraries; the devel symlinks are not needed.
  GST_LIBS := -l:libgstaudio-1.0.so.0 -l:libgstbase-1.0.so.0 -l:libgstreamer-1.0.so.0 $(shell $(PKG_CONFIG) --libs gobject-2.0 glib-2.0)
endif

all: $(PLUGIN)

$(PLUGIN): gstsyrinx.c $(DIST)/include/syrinx.h $(DIST)/lib/libsyrinx.so
	@if [ "$(HAVE_GST_DEVEL)" != yes ] && [ ! -d $(HEADERS) ]; then \
		echo "GStreamer headers not found: install gstreamer1-devel and gstreamer1-plugins-base-devel, or run 'make headers'"; exit 1; fi
	mkdir -p $(BUILD)
	$(CC) -std=gnu11 -O2 -Wall -Wextra -fPIC -shared \
		-DSYRINX_PLUGIN_VERSION=\"$(VERSION)\" $(GST_CFLAGS) -I$(DIST)/include \
		gstsyrinx.c -o $@ \
		-L$(DIST)/lib -lsyrinx $(GST_LIBS) -Wl,-rpath,'$$ORIGIN' -Wl,-rpath,$(DIST)/lib
	cp $(DIST)/lib/libsyrinx.so $(BUILD)/

headers:
	mkdir -p $(BUILD)/headers
	cd $(BUILD)/headers && dnf download --arch x86_64 gstreamer1-devel gstreamer1-plugins-base-devel
	cd $(BUILD)/headers && for r in *.rpm; do rpm2cpio $$r | cpio -idm --quiet; done
	@echo "headers under $(BUILD)/headers/usr/include/gstreamer-1.0"

USER_PLUGINSDIR := $(HOME)/.local/share/gstreamer-1.0/plugins

install-user: $(PLUGIN)
	install -D -m 755 $(PLUGIN) $(USER_PLUGINSDIR)/libgstsyrinx.so
	install -D -m 755 $(DIST)/lib/libsyrinx.so $(USER_PLUGINSDIR)/libsyrinx.so
	rm -rf $(HOME)/.cache/gstreamer-1.0/registry.*.bin
	@# The MIME type, so the file manager knows a .syr is audio (shared with the VLC module).
	-xdg-mime install --mode user ../vlc/syrinx-mime.xml 2>/dev/null && update-mime-database $(HOME)/.local/share/mime 2>/dev/null
	@if [ -f /usr/share/applications/org.gnome.Decibels.desktop ]; then \
		xdg-mime default org.gnome.Decibels.desktop audio/x-syrinx && echo "audio/x-syrinx now opens in Audio Player (Decibels)"; fi
	@gst-inspect-1.0 syrinxdec >/dev/null 2>&1 && echo "installed: gst-inspect-1.0 syrinxdec" || echo "installed, but gst-inspect-1.0 cannot see syrinxdec"

uninstall-user:
	rm -f $(USER_PLUGINSDIR)/libgstsyrinx.so $(USER_PLUGINSDIR)/libsyrinx.so
	rm -rf $(HOME)/.cache/gstreamer-1.0/registry.*.bin

clean:
	rm -rf $(BUILD)

.PHONY: all headers install-user uninstall-user clean

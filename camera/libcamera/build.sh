#!/bin/sh
# SPDX-License-Identifier: MIT
# Build and install the TB520FU camera stack on the tablet (run as the desktop user; uses sudo).
#
# libcamera upstream master (0.8.0) pinned below + 000*.patch, installed to /opt/libcamera-tb520fu
# next to the old 0.7.1 build in /opt/libcamera-y700 (different soname, no conflict). The pipewire
# libcamera plugin is rebuilt against it and found first through SPA_PLUGIN_DIR.
#   0001  simple: native manual focus control (Osilvfe, iceland-mainline; FocusAbsolute)
#   0002  libipa: GC13A0/GC13A2/SC202CS/SC820CS sensor helpers (gain code 1024 = 1x)
#   0003  CameraLens: power the VCM only while streaming
#   0004  softisp Adjust: default gamma/contrast/saturation from the tuning file
#   0005  software ISP focus figure of merit (centre half, normalised by brightness)
#   0006  simple: contrast-detect autofocus (continuous by default; auto/manual too)
# Tuning files: ../tuning/*.yaml -> /opt/libcamera-tb520fu/share/libcamera/ipa/softisp/
set -e
LIBCAMERA_REV=08c79822a408ea7dbe43320a8d795d2747ea6d33
PIPEWIRE_VER=1.6.9-2~bpo13+1
PREFIX=/opt/libcamera-tb520fu
HERE=$(dirname "$(realpath "$0")")
W=$HOME/build/libcamera-tb520fu
mkdir -p "$W" && cd "$W"

sudo apt-get install -y git meson ninja-build pkg-config python3-yaml python3-ply python3-jinja2 \
  libyaml-dev libudev-dev libevent-dev libdrm-dev libjpeg-dev libtiff-dev libssl-dev libgnutls28-dev \
  libegl-dev libgles-dev libepoxy-dev libglib2.0-dev libdw-dev libunwind-dev libspa-0.2-dev libpipewire-0.3-dev

rm -rf libcamera builddir
git clone https://github.com/libcamera-org/libcamera.git libcamera
git -C libcamera checkout --detach "$LIBCAMERA_REV"
for p in "$HERE"/000*.patch; do git -C libcamera apply "$p"; done
meson setup builddir libcamera --prefix=$PREFIX --buildtype=release -Dpipelines=simple -Dipas=softisp \
  -Dsoftisp-gpu=enabled -Dgstreamer=disabled -Dqcam=disabled -Dpycamera=disabled -Ddocumentation=disabled \
  -Dtest=false -Dtracing=disabled -Dlc-compliance=disabled -Dcam=enabled
ninja -C builddir
sudo ninja -C builddir install
sudo install -m644 "$HERE"/../tuning/*.yaml $PREFIX/share/libcamera/ipa/softisp/
echo $PREFIX/lib | sudo tee /etc/ld.so.conf.d/tb520fu-libcamera.conf >/dev/null
sudo ldconfig

# pipewire libcamera plugin against libcamera 0.8 (same pipewire version as the system).
rm -rf pw pwbuild && mkdir -p pw
apt-get source --download-only "pipewire=$PIPEWIRE_VER"
tar xzf pipewire_*.orig.tar.gz -C pw --strip-components=1
tar xJf pipewire_"$PIPEWIRE_VER".debian.tar.xz -C pw
(cd pw && for p in $(grep -v '^#' debian/patches/series); do patch -p1 -s < debian/patches/$p; done \
  && patch -p1 < "$HERE"/pipewire-1.6.9-libcamera-0.8.patch)
PKG_CONFIG_PATH=$PREFIX/lib/pkgconfig meson setup pwbuild pw -Dauto_features=disabled -Dspa-plugins=enabled \
  -Dlibcamera=enabled -Dsession-managers=[] -Dpipewire-v4l2=disabled -Dexamples=disabled -Dtests=disabled \
  -Dman=disabled -Ddocs=disabled
ninja -C pwbuild spa/plugins/libcamera/libspa-libcamera.so
sudo install -Dm644 pwbuild/spa/plugins/libcamera/libspa-libcamera.so $PREFIX/lib/spa-0.2/libcamera/libspa-libcamera.so
sudo strip --strip-debug $PREFIX/lib/spa-0.2/libcamera/libspa-libcamera.so

for s in pipewire wireplumber; do
  sudo mkdir -p /etc/systemd/user/$s.service.d
  printf '[Service]\n# libcamera 0.8 (TB520FU build, %s): its pipewire plugin goes first.\nEnvironment=SPA_PLUGIN_DIR=%s/lib/spa-0.2:/usr/lib/aarch64-linux-gnu/spa-0.2\n' \
    $PREFIX $PREFIX | sudo tee /etc/systemd/user/$s.service.d/60-tb520fu-libcamera.conf >/dev/null
done
# Distinct names for the three cameras (two are both "back").
sudo install -Dm644 "$HERE"/51-tb520fu-cameras.conf /etc/wireplumber/wireplumber.conf.d/51-tb520fu-cameras.conf
systemctl --user daemon-reload
systemctl --user restart wireplumber pipewire

#!/bin/sh
# SPDX-License-Identifier: MIT
# Build and install TB520FU niri with HDR (2026-11-18 onwards; replaces build-niri.sh).
#
# Base: losnoco/niri + losnoco/smithay `color-management-features` (dividebysandwich's HDR work:
# wp_color_management_v1, PQ blend space, HDR10 signalling), pinned below. On top, hdr/000*.patch:
#   0001  panel EDR — HDR on the SDR-only DSI panel via backlight headroom (tb520fu-daemon `edr`),
#         plus a fix for the 10-bit probe leaving the CRTC disabled (black screen after login)
#   0002–0005  the TB520FU patches formerly applied to v26.04 (0001–0004 in this directory):
#         multi-finger pinch/swipe + TouchGateShm, hardware CTM night light / reading mode and
#         clean Overview, touch window pill/seam/edge manipulation + touchpad gestures
#         (ported to this smithay: TouchGrab methods lost their `seq` argument)
# Needs Rust >= 1.87 (Debian 13 ships 1.85; trixie-backports has 1.96). Full build ~7 min on SM8650.
# Config: output "DSI-1" { hdr { panel-peak-luminance 650 } } (900 with tb520fu-daemon's HBM headroom).
set -e
NIRI_REV=8d2ecca685b768783b724ab5a3cd533b4d06be40
SMITHAY_REV=b2d26fe77c99c0919d66d1b030bde2064cb02fcb

apt-get install -y git clang pkg-config libudev-dev libgbm-dev libxkbcommon-dev libegl-dev \
  libwayland-dev libinput-dev libdbus-1-dev libsystemd-dev libseat-dev libpango1.0-dev libdisplay-info-dev \
  libpipewire-0.3-dev/trixie-backports libspa-0.2-dev/trixie-backports
apt-get install -y -t trixie-backports rustc cargo
mkdir -p /root/.cargo /root/build/niri-hdr
cat > /root/.cargo/config.toml <<'C'
[source.crates-io]
replace-with = "ustc"
[source.ustc]
registry = "sparse+https://mirrors.ustc.edu.cn/crates.io-index/"
[net]
git-fetch-with-cli = true
C
HERE=$(dirname "$(realpath "$0")")
cd /root/build/niri-hdr; rm -rf niri smithay
git clone --branch color-management-features https://github.com/losnoco/smithay.git smithay
git -C smithay checkout --detach "$SMITHAY_REV"
git clone --branch color-management-features https://github.com/losnoco/niri.git niri
cd niri
git checkout --detach "$NIRI_REV"
for p in "$HERE"/hdr/0*.patch; do git apply "$p"; done
# Cargo.toml already has [patch] smithay = { path = "../smithay" }.
cargo build --release --locked
R=resources
[ -e /usr/local/bin/niri ] && cp -a /usr/local/bin/niri /usr/local/bin/niri.prev
install -Dm755 target/release/niri /usr/local/bin/niri.new && strip /usr/local/bin/niri.new
mv -f /usr/local/bin/niri.new /usr/local/bin/niri    # rename: the running binary stays intact
install -Dm755 $R/niri-session /usr/local/bin/niri-session
install -Dm644 $R/niri.desktop /usr/share/wayland-sessions/niri.desktop
install -Dm644 $R/niri.service /usr/local/lib/systemd/user/niri.service
install -Dm644 $R/niri-shutdown.target /usr/local/lib/systemd/user/niri-shutdown.target
install -Dm644 $R/niri-portals.conf /usr/local/share/xdg-desktop-portal/niri-portals.conf

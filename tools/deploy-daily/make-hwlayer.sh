#!/bin/sh
# SPDX-License-Identifier: MIT
# make-hwlayer.sh — run on the TB520FU *test* system (@main) as root.
# Packs everything a clean Debian 13 needs on top of its packages to drive this tablet:
#   hwlayer.tar.zst   hardware layer files picked from rootfs-capture's custom-files.list by an
#                     allow-list (no GNOME/GDM, no generated files, no secrets) + /usr/lib/modules
#   secrets.tar.zst   Wi-Fi connections, Bluetooth pairings, the user's password hash (mode 600)
#   bluez-debs.tar    self-built BlueZ 5.88 packages (CONFIG_BT_LE / LE Audio fixes, see README)
# Needs /root/capture/custom-files.list from tools/rootfs-capture.sh.
set -eu
O=${1:-/root/hwlayer}
L=/root/capture/custom-files.list
[ -s "$L" ] || { echo "run rootfs-capture.sh first" >&2; exit 1; }
rm -rf "$O"; mkdir -p "$O"; chmod 700 "$O"
export LC_ALL=C

KEEP='^/usr/share/qcom/|^/usr/local/share/y700-sns/|^/usr/lib/firmware/|^/usr/local/(sbin|bin|lib|libexec)/|^/usr/local/share/xdg-desktop-portal/|^/opt/libcamera-y700/'
KEEP="$KEEP"'|^/usr/lib/aarch64-linux-gnu/(libhexagonrpc|libssc|pkgconfig/libssc)'
KEEP="$KEEP"'|^/usr/lib/systemd/system/qcom-sns-init\.service$|^/usr/lib/udev/rules\.d/80-tb321fu-qcom-sns\.rules$|^/usr/libexec/qcom-sns'
KEEP="$KEEP"'|^/usr/share/alsa/ucm2/|^/usr/share/libcamera/ipa/|^/usr/share/wireplumber/'
KEEP="$KEEP"'|^/etc/(bluetooth/main\.conf|dbus-1/system\.d/net\.hadess|ld\.so\.conf\.d/y700|modprobe\.d/tb520fu|modules-load\.d/|udev/(hwdb\.d|rules\.d)/|wireplumber/|fonts/conf\.d/)'
KEEP="$KEEP"'|^/etc/(systemd/logind\.conf\.d/tb520fu|ssh/sshd_config\.d/tb520fu|apt/sources\.list\.d/(trixie-backports|trixie-backports-src)\.list)'
KEEP="$KEEP"'|^/etc/systemd/system/(qcom-sns|iio-sensor-proxy\.service\.d/|tb520fu-(icc-sync|pen-charge|power-key|sns-sleep|usb-console|usb-gadget)\.service|y700-audio-card-guard)'
KEEP="$KEEP"'|^/etc/systemd/user/(tb520fu-|pipewire\.service\.d/|wireplumber\.service\.d/)'
DROP='^/usr/local/sbin/(disp-capture\.sh|usb-state-dump\.sh)$'

grep -E "$KEEP" "$L" | grep -vE "$DROP" > "$O/hwlayer.list"
# swapped-in replacements of packaged files (re-diverted by deploy-daily.sh)
printf '%s\n' /usr/libexec/iio-sensor-proxy /usr/lib/aarch64-linux-gnu/spa-0.2/libcamera/libspa-libcamera.so >> "$O/hwlayer.list"
# sensor registry re-parsed for Linux (lives under /var, so rootfs-capture does not list it)
find /var/lib/qcom-sns >> "$O/hwlayer.list"
echo /var/lib/alsa/asound.state >> "$O/hwlayer.list"   # saved mixer state
# the full module tree of the running kernel (one kernel in boot_a, so the daily system needs the same)
find /usr/lib/modules/7.3.3 >> "$O/hwlayer.list"
sort -u -o "$O/hwlayer.list" "$O/hwlayer.list"
tar --no-recursion -cf - -T "$O/hwlayer.list" 2>"$O/hwlayer.err" | zstd -q -19 -T0 > "$O/hwlayer.tar.zst"

{ ls -d /etc/NetworkManager/system-connections/* 2>/dev/null; find /var/lib/bluetooth 2>/dev/null; } > "$O/secrets.list"
getent shadow debian | cut -d: -f2 > "$O/debian.pwhash"
tar --no-recursion -cf - -T "$O/secrets.list" 2>>"$O/hwlayer.err" | zstd -q -T0 > "$O/secrets.tar.zst"
chmod 600 "$O/secrets.tar.zst" "$O/debian.pwhash"

tar -cf "$O/bluez-debs.tar" -C /root/bluez-bpo $(cd /root/bluez-bpo && ls bluez_*.deb bluez-obexd_*.deb libbluetooth3_*.deb bluetooth_*.deb)

cd "$O"; wc -l hwlayer.list secrets.list > counts.txt; sha256sum *.tar *.zst > SHA256SUMS; echo "$O"

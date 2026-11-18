#!/bin/sh
# SPDX-License-Identifier: MIT
# deploy-daily.sh — build the TB520FU daily system (Debian 13 + Niri, no GNOME) into a mounted root.
# Run as root on a working TB520FU Linux (the test system), chrooting into the target:
#   deploy-daily.sh <target-root> <src-dir>
#     target-root  @daily mounted, with @home mounted at <target-root>/home
#     src-dir      make-hwlayer.sh output (hwlayer.tar.zst, secrets.tar.zst, bluez-debs.tar,
#                  debian.pwhash); niri config, shell and user units come from ../../desktop
# Re-runnable: every step either checks first or overwrites with the same content.
set -eu
T=$(realpath "$1"); SRC=$(realpath "$2")
DESKDIR=$(realpath "$(dirname "$0")/../../desktop")   # shell, niri config, user units (in the repo)
OVERLAY=$(realpath "$(dirname "$0")/../../daily-overlay")   # daily-system config: MACs, speaker protection
[ -x "$T/usr/bin/apt-get" ] || { echo "no Debian root at $T" >&2; exit 1; }
mountpoint -q "$T/home" || { echo "mount @home at $T/home first" >&2; exit 1; }
for f in hwlayer.tar.zst secrets.tar.zst bluez-debs.tar debian.pwhash; do
	[ -s "$SRC/$f" ] || { echo "missing $SRC/$f" >&2; exit 1; }
done
log() { echo "== $*"; }
ch() { chroot "$T" /usr/bin/env -i PATH=/usr/sbin:/usr/bin:/sbin:/bin HOME=/root LANG=C.UTF-8 DEBIAN_FRONTEND=noninteractive "$@"; }

# ---------- chroot plumbing
mountpoint -q "$T/proc" || mount -t proc proc "$T/proc"
mountpoint -q "$T/sys" || mount --rbind /sys "$T/sys"
mountpoint -q "$T/dev" || mount --rbind /dev "$T/dev"
mountpoint -q "$T/run" || mount -t tmpfs tmpfs "$T/run"
trap 'umount -R "$T/run" "$T/dev" "$T/sys" "$T/proc" 2>/dev/null || true' EXIT
rm -f "$T/etc/resolv.conf"; cat /etc/resolv.conf > "$T/etc/resolv.conf"

# ---------- 1. apt sources (backports only where asked for)
log apt sources
M=http://mirrors.ustc.edu.cn
cat > "$T/etc/apt/sources.list" <<EOF
deb $M/debian trixie main contrib non-free non-free-firmware
deb $M/debian trixie-updates main contrib non-free non-free-firmware
deb $M/debian-security trixie-security main contrib non-free non-free-firmware
EOF
echo "deb $M/debian trixie-backports main contrib non-free non-free-firmware" > "$T/etc/apt/sources.list.d/trixie-backports.list"
ch apt-get -q update
ch apt-get -y -q full-upgrade

# ---------- 2. packages
log packages
BASE="sudo locales tzdata ca-certificates nano less vim-tiny bash-completion openssh-server network-manager
 wpasupplicant iw rfkill wireless-regdb iproute2 iputils-ping btrfs-progs gdisk zstd usbutils pciutils
 v4l-utils alsa-utils alsa-ucm-conf pulseaudio-utils kmod strace cron logrotate nftables avahi-daemon
 libnss-mdns systemd-timesyncd dbus-user-session polkitd udisks2 power-profiles-daemon upower
 iio-sensor-proxy libqrtr-glib0 libprotobuf-c1 adbd rtkit
 libevent-2.1-7t64 libevent-pthreads-2.1-7t64"   # libevent: /opt/libcamera-y700 tools
GPU="libgl1-mesa-dri libegl-mesa0 libgles2 mesa-vulkan-drivers mesa-va-drivers libgbm1"
DESK="greetd tuigreet foot fuzzel squeekboard wvkbd swayidle wlopm wlr-randr xdg-desktop-portal
 xdg-desktop-portal-gnome xdg-desktop-portal-gtk fonts-noto-cjk fonts-noto-color-emoji fonts-noto-mono
 qt6-wayland dconf-gsettings-backend ffmpeg gstreamer1.0-tools
 fonts-material-design-icons-iconfont libnotify-bin grim wtype"   # icon font of the shell; notify-send; screenshots
NIRI_RT="libseat1 libinput10 libdisplay-info2 libpango-1.0-0 libpangocairo-1.0-0 libxkbcommon0
 libwacom9 libgudev-1.0-0 libmbim-glib4 libqmi-glib5"
BPO="pipewire pipewire-audio pipewire-alsa pipewire-pulse pipewire-bin wireplumber libspa-0.2-bluetooth
 libspa-0.2-libcamera libcamera0.7 libcamera-ipa gstreamer1.0-libcamera gstreamer1.0-pipewire quickshell"
ch apt-get install -y -q --no-install-recommends $BASE $GPU $DESK $NIRI_RT
ch apt-get install -y -q --no-install-recommends -t trixie-backports $BPO
# self-built BlueZ 5.88 (LE Audio / pen fixes); held so trixie's 5.82 never replaces it
rm -rf "$T/tmp/bluez"; mkdir -p "$T/tmp/bluez"; tar -xf "$SRC/bluez-debs.tar" -C "$T/tmp/bluez"
ch sh -c 'apt-get install -y -q --no-install-recommends /tmp/bluez/*.deb'
ch apt-mark hold bluez bluez-obexd libbluetooth3 bluetooth >/dev/null
rm -rf "$T/tmp/bluez"

# ---------- 3. hardware layer (divert the two packaged files it replaces first)
log hardware layer
ch dpkg-divert --local --rename --divert /usr/libexec/iio-sensor-proxy.distrib --add /usr/libexec/iio-sensor-proxy
ch dpkg-divert --local --rename --divert /usr/lib/aarch64-linux-gnu/spa-0.2/libcamera/libspa-libcamera.so.debian \
	--add /usr/lib/aarch64-linux-gnu/spa-0.2/libcamera/libspa-libcamera.so
zstd -dc "$SRC/hwlayer.tar.zst" | tar -xpf - --numeric-owner -C "$T"
chmod 644 "$T"/etc/systemd/system/*.service "$T"/etc/systemd/user/*.service 2>/dev/null || true   # some came over with +x
for x in "$SRC"/extra-*.tar.zst; do [ -s "$x" ] && zstd -dc "$x" | tar -xpf - --numeric-owner -C "$T"; done
# files the 11-08 @pristine base still carries but the test system dropped long ago: an old
# y700 sensor-proxy path (needs a nonexistent y700-sns-init.service), old libcamera drop-ins
# that fight the current 60-tb520fu ones, and an enabled adbd that fights the USB gadget
rm -f "$T/etc/systemd/system/iio-sensor-proxy.service.d/10-y700-ssc.conf" \
	"$T/etc/systemd/user/pipewire.service.d/50-y700-libcamera-ipa.conf" \
	"$T/etc/systemd/user/pipewire.service.d/60-y700-libcamera-paths.conf" \
	"$T/etc/systemd/user/wireplumber.service.d/60-y700-libcamera-paths.conf" \
	"$T/etc/udev/rules.d/80-y700-iio-sensor-proxy.rules" \
	"$T/etc/systemd/system/multi-user.target.wants/adbd.service"
ch ldconfig
ch systemd-hwdb update
ch depmod 7.3.3

# daily-system config kept in the repo (factory Wi-Fi MAC .link + NM preserve, speaker high-pass and
# 0 dB cap); the Bluetooth address goes into the device tree at kernel build (tools/kernel/set-bdaddr.sh)
( cd "$OVERLAY" && find . -type d -exec install -d -m 755 "$T/{}" \; && find . -type f ! -name '*.in' -exec install -m 644 {} "$T/{}" \; )
# Per-device values (device.conf.example): *.in templates get @WIFI_MAC@ filled in; without
# a Wi-Fi MAC the .link is skipped and the driver picks a random address each boot.
WIFI_MAC=${TB520FU_WIFI_MAC:-$(sed -n 's/^WIFI_MAC=//p' "$(dirname "$(realpath "$0")")/../../device.local.conf" 2>/dev/null | tr -d ' \r')}
( cd "$OVERLAY" && find . -type f -name '*.in' ) | while read -r f; do
	if [ -n "$WIFI_MAC" ]; then
		sed "s/@WIFI_MAC@/$WIFI_MAC/g" "$OVERLAY/$f" > "$T/${f%.in}" && chmod 644 "$T/${f%.in}"
	else
		echo "   no WIFI_MAC in device.local.conf: skipping ${f%.in}" >&2
	fi
done

# keep kernel messages off the text consoles (they garble tuigreet)
echo "kernel.printk = 3 4 1 3" > "$T/etc/sysctl.d/10-tb520fu-quiet-console.conf"

# ---------- 4. host identity, locale, time
log identity
echo yoga-tb520fu > "$T/etc/hostname"
grep -q yoga-tb520fu "$T/etc/hosts" || printf '127.0.1.1\tyoga-tb520fu\n' >> "$T/etc/hosts"
TZ=$(readlink /etc/localtime | sed 's#.*/zoneinfo/##'); [ -n "$TZ" ] && ch ln -sf "/usr/share/zoneinfo/$TZ" /etc/localtime
sed -i -E 's/^# *(en_US.UTF-8|zh_CN.UTF-8)/\1/' "$T/etc/locale.gen"; ch locale-gen >/dev/null

# ---------- 5. user (home lives on @home)
log user
ch id debian >/dev/null 2>&1 || ch useradd -m -u 1000 -s /bin/bash -G sudo,users,video,render,input,audio debian
ch usermod -p "$(cat "$SRC/debian.pwhash")" debian
# remote access (user decision 2026-11-13): SSH as debian by key only, no root login, no passwords;
# debian runs sudo without a password. The key is the one the source system accepts (debian's on a
# locked-down daily system, root's on the old test system).
K=/home/debian/.ssh/authorized_keys; [ -s "$K" ] || K=/root/.ssh/authorized_keys
install -d -m 700 "$T/home/debian/.ssh"; cat "$K" > "$T/home/debian/.ssh/authorized_keys"
chmod 600 "$T/home/debian/.ssh/authorized_keys"
printf '%s\n' "# TB520FU: the debian user runs sudo without a password (user decision 2026-11-13;" \
	"# SSH logs in as debian by key only, root SSH is off)" "debian ALL=(ALL:ALL) NOPASSWD: ALL" > "$T/etc/sudoers.d/90-tb520fu-debian"
chmod 440 "$T/etc/sudoers.d/90-tb520fu-debian"; ch visudo -c >/dev/null
rm -f "$T/etc/ssh/sshd_config.d/tb520fu-debug.conf"   # bring-up config from the hardware layer (root by key)
printf '%s\n' "# TB520FU daily system: key login only, no root (log in as debian, use sudo)" \
	"PermitRootLogin no" "PasswordAuthentication no" "KbdInteractiveAuthentication no" > "$T/etc/ssh/sshd_config.d/tb520fu.conf"
rm -rf "$T/root/.ssh"

# ---------- 6. Wi-Fi and Bluetooth pairings
log secrets
zstd -dc "$SRC/secrets.tar.zst" | tar -xpf - --numeric-owner -C "$T"
chmod 600 "$T"/etc/NetworkManager/system-connections/* 2>/dev/null || true

# ---------- 7. fstab (the initramfs mounts / itself; this is for remounts and /home)
log fstab
DEV=$(findmnt -no SOURCE --target "$T" | sed 's/\[.*//'); UUID=$(blkid -s UUID -o value "$DEV")
O=noatime,compress=zstd:1,discard=async
cat > "$T/etc/fstab" <<EOF
UUID=$UUID / btrfs subvol=@daily,$O 0 0
UUID=$UUID /home btrfs subvol=@home,$O 0 0
EOF

# ---------- 8. services
log services
ch systemctl enable NetworkManager bluetooth ssh systemd-timesyncd avahi-daemon qcom-sns-init \
	tb520fu-icc-sync tb520fu-pen-charge tb520fu-power-key tb520fu-sns-sleep \
	tb520fu-usb-gadget serial-getty@ttyGS0 greetd >/dev/null 2>&1 || true
# USB serial gets a login prompt (debian's password), not the bring-up root shell
ch systemctl disable tb520fu-usb-console >/dev/null 2>&1 || true
ch systemctl --global enable pipewire.socket pipewire-pulse.socket wireplumber.service filter-chain.service tb520fu-mute-led.service >/dev/null 2>&1 || true
ch systemctl set-default graphical.target >/dev/null
# user units are enabled globally, so keep them out of system accounts: the greeter (its WirePlumber
# would open the cameras and hold the focus motor powered) and root over SSH (failed-unit spam)
for u in pipewire.socket pipewire.service pipewire-pulse.socket pipewire-pulse.service wireplumber.service \
	filter-chain.service tb520fu-mute-led.service; do
	install -d "$T/etc/systemd/user/$u.d"
	printf '[Unit]\nConditionUser=!@system\n' > "$T/etc/systemd/user/$u.d/10-not-greeter.conf"
done

# ---------- 9. login: greetd + tuigreet -> niri
log greetd
cat > "$T/etc/greetd/config.toml" <<'EOF'
[terminal]
vt = 7   # Debian greetd.service conflicts with getty@tty7 only; tty1 keeps its getty

[default_session]
command = "tuigreet --time --remember --asterisks --cmd niri-session"
user = "_greetd"
EOF

# ---------- 10. niri config and the Quickshell shell for the user
log niri config and shell
H="$T/home/debian/.config"; install -d "$H/niri"
if [ -s "$DESKDIR/niri/config.kdl" ]; then cp "$DESKDIR/niri/config.kdl" "$H/niri/config.kdl"; else cp "$SRC/niri-config.kdl" "$H/niri/config.kdl"; fi
grep -q squeekboard "$H/niri/config.kdl" || cat >> "$H/niri/config.kdl" <<'EOF2'

// TB520FU: on-screen keyboard (shows when a text field gets focus)
spawn-at-startup "sh" "-c" "gsettings set org.gnome.desktop.a11y.applications screen-keyboard-enabled true; exec squeekboard"
EOF2
rm -rf "$H/quickshell"; cp -r "$DESKDIR/quickshell" "$H/quickshell"
install -m 644 "$DESKDIR/systemd/tb520fu-shell.service" "$T/etc/systemd/user/tb520fu-shell.service"
ch systemctl --global enable tb520fu-shell.service >/dev/null 2>&1 || true
ch chown -R debian:debian /home/debian

ch apt-get -q clean
log done

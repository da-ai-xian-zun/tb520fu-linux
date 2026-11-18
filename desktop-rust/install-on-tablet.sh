#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
# One-shot installer (and --rollback script) for the TB520FU 100% Pure-Rust OriginOS Desktop Suite
# Target: Lenovo Yoga Pad Pro 12.7 (TB520FU, SM8650) running @daily Debian 13 (Trixie) arm64 + Niri v26.04.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TARGET_USER="${TB520FU_USER:-${SUDO_USER:-debian}}"
if [[ "$TARGET_USER" == "root" ]] || ! id "$TARGET_USER" >/dev/null 2>&1; then
    TARGET_USER="$(awk -F: '$3 == 1000 { print $1; exit }' /etc/passwd || echo debian)"
fi
TARGET_UID="$(id -u "$TARGET_USER")"
TARGET_HOME="$(getent passwd "$TARGET_USER" | cut -d: -f6)"

if [[ "${EUID}" -ne 0 ]]; then
    echo "Please run as root (e.g. sudo $0 $*)" >&2
    exit 1
fi

WITH_DEPS=0
WITH_ADDON=0
ROLLBACK=0
for arg in "$@"; do
    case "$arg" in
        --with-deps) WITH_DEPS=1 ;;
        --with-fcitx-addon) WITH_ADDON=1 ;;
        --rollback) ROLLBACK=1 ;;
    esac
done

run_user_systemctl() {
    if [[ -d "/run/user/${TARGET_UID}" ]]; then
        sudo -u "$TARGET_USER" XDG_RUNTIME_DIR="/run/user/${TARGET_UID}" \
            DBUS_SESSION_BUS_ADDRESS="unix:path=/run/user/${TARGET_UID}/bus" \
            systemctl --user "$@" || true
    fi
}

if [[ "$ROLLBACK" -eq 1 ]]; then
    echo "==> Rolling back TB520FU Rust Desktop Suite..."
    systemctl disable --now tb520fu-daemon.service 2>/dev/null || true
    run_user_systemctl disable --now tb520fu-shell.service tb520fu-osk.service tb520fu-idle.service

    rm -f /etc/systemd/system/tb520fu-daemon.service /etc/udev/rules.d/99-tb520fu-haptics.rules
    rm -f /etc/polkit-1/rules.d/49-tb520fu-settings.rules
    rm -f /usr/local/bin/tb520fu-settings /usr/share/applications/fyi.tb520fu.Settings.desktop
    rm -f /usr/share/icons/hicolor/scalable/apps/fyi.tb520fu.Settings.svg
    udevadm control --reload-rules 2>/dev/null || true
    rm -f /etc/systemd/user/tb520fu-osk.service /usr/local/lib/systemd/user/tb520fu-osk.service
    rm -f /etc/systemd/user/tb520fu-idle.service /usr/local/lib/systemd/user/tb520fu-idle.service
    rm -f /usr/local/lib/systemd/user/tb520fu-shell.service
    if [[ -f /etc/systemd/user/tb520fu-shell.service.quickshell-bak ]]; then
        mv -f /etc/systemd/user/tb520fu-shell.service.quickshell-bak /etc/systemd/user/tb520fu-shell.service
    else
        rm -f /etc/systemd/user/tb520fu-shell.service
    fi
    rm -f "${TARGET_HOME}/.config/systemd/user/niri.service.wants/tb520fu-"*.service
    rm -f "${TARGET_HOME}/.config/systemd/user/graphical-session.target.wants/tb520fu-"*.service

    if [[ -f /etc/greetd/config.toml.tuigreet-bak ]]; then
        mv -f /etc/greetd/config.toml.tuigreet-bak /etc/greetd/config.toml
        echo "Restored /etc/greetd/config.toml (tuigreet)"
    fi
    rm -f /etc/greetd/niri-greeter.kdl /usr/local/bin/tb520fu-greeter-session /usr/local/bin/tb520fu-user-session

    systemctl daemon-reload
    run_user_systemctl daemon-reload

    if [[ -f "${TARGET_HOME}/.config/niri/config.kdl.pre-rust" ]]; then
        cp -a "${TARGET_HOME}/.config/niri/config.kdl.pre-rust" "${TARGET_HOME}/.config/niri/config.kdl"
        echo "Restored ${TARGET_HOME}/.config/niri/config.kdl"
    fi

    for svc in tb520fu-power-key.service tb520fu-pen-charge.service; do
        if systemctl list-unit-files "$svc" >/dev/null 2>&1; then
            systemctl enable --now "$svc" || true
        fi
    done
    if [[ -f /etc/systemd/user/tb520fu-mute-led.service ]]; then
        mkdir -p /etc/systemd/user/default.target.wants
        ln -sf /etc/systemd/user/tb520fu-mute-led.service /etc/systemd/user/default.target.wants/tb520fu-mute-led.service
    fi
    run_user_systemctl enable --now tb520fu-mute-led.service tb520fu-shell.service
    echo "==> Rollback complete."
    exit 0
fi

ARCH="$(uname -m)"
if [[ "$ARCH" != "aarch64" ]]; then
    echo "ERROR: Expected aarch64 tablet target, got ${ARCH}." >&2
    exit 1
fi

for bin in tb520fu-daemon tb520fu-osk tb520fu-shell tb520fu-settings; do
    if [[ ! -x "${HERE}/dist/aarch64/${bin}" ]]; then
        echo "ERROR: Missing prebuilt binary ${HERE}/dist/aarch64/${bin}" >&2
        exit 1
    fi
done

echo "==> [1/7] Ensuring runtime helper packages (Turnip Vulkan ICD, fcitx5, pinyin, brightnessctl, playerctl)..."
if [[ "$WITH_DEPS" -eq 1 ]]; then
    DEBIAN_FRONTEND=noninteractive apt-get update -qq || true
    DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        mesa-vulkan-drivers libvulkan1 \
        fcitx5 fcitx5-chinese-addons fcitx5-modules \
        wl-clipboard grim slurp brightnessctl playerctl swayidle \
        fonts-noto-cjk fonts-noto-color-emoji
fi

usermod -aG input,video,render "$TARGET_USER" 2>/dev/null || true
cat > /etc/udev/rules.d/99-tb520fu-haptics.rules <<'EOF'
SUBSYSTEM=="input", KERNEL=="event*", ATTRS{name}=="*aw86927*", MODE="0660", GROUP="input"
SUBSYSTEM=="input", KERNEL=="event*", ATTRS{name}=="*Stylus*|*Pen*", MODE="0660", GROUP="input"
EOF
udevadm control --reload-rules 2>/dev/null || true
udevadm trigger --subsystem-match=input 2>/dev/null || true

echo "==> [2/7] Installing pure-Rust binaries & stock feather wallpaper..."
for bin in tb520fu-daemon tb520fu-osk tb520fu-shell tb520fu-settings; do
    install -Dm755 "${HERE}/dist/aarch64/${bin}" "/usr/local/bin/${bin}.new"
    mv -f "/usr/local/bin/${bin}.new" "/usr/local/bin/${bin}"
done
if [[ -f "${HERE}/assets/fyi.tb520fu.Settings.desktop" ]]; then
    install -Dm644 "${HERE}/assets/fyi.tb520fu.Settings.desktop" /usr/share/applications/fyi.tb520fu.Settings.desktop
fi
if [[ -f "${HERE}/assets/fyi.tb520fu.Settings.svg" ]]; then
    install -Dm644 "${HERE}/assets/fyi.tb520fu.Settings.svg" /usr/share/icons/hicolor/scalable/apps/fyi.tb520fu.Settings.svg
fi
if [[ -f "${HERE}/polkit/49-tb520fu-settings.rules" ]]; then
    install -Dm644 "${HERE}/polkit/49-tb520fu-settings.rules" /etc/polkit-1/rules.d/49-tb520fu-settings.rules
fi
if [[ -f "${HERE}/assets/wallpaper.jpg" ]]; then
    install -Dm644 "${HERE}/assets/wallpaper.jpg" /usr/share/tb520fu/wallpaper.jpg
fi
if [[ -f "${HERE}/../boot-switch/tools/tb520fu-bootflag" ]]; then
    install -Dm755 "${HERE}/../boot-switch/tools/tb520fu-bootflag" /usr/local/sbin/tb520fu-bootflag
    install -Dm755 "${HERE}/../boot-switch/tools/reboot-to-android" /usr/local/sbin/reboot-to-android
fi

echo "==> [3/7] Configuring greetd graphical PAM login greeter..."
if [[ -f /etc/greetd/config.toml ]] && ! grep -q "tb520fu-greeter-session" /etc/greetd/config.toml; then
    cp -a /etc/greetd/config.toml /etc/greetd/config.toml.tuigreet-bak
fi
install -Dm755 "${HERE}/greetd/tb520fu-greeter-session" /usr/local/bin/tb520fu-greeter-session
install -Dm755 "${HERE}/greetd/tb520fu-user-session" /usr/local/bin/tb520fu-user-session
install -Dm644 "${HERE}/greetd/niri-greeter.kdl" /etc/greetd/niri-greeter.kdl
install -Dm644 "${HERE}/greetd/greetd-config.toml" /etc/greetd/config.toml
if id _greetd >/dev/null 2>&1; then
    usermod -aG video,render,input _greetd 2>/dev/null || true
fi

echo "==> [4/7] Configuring systemd system & user units (5-min idle lock, NO auto-suspend)..."
if [[ -f /etc/systemd/user/tb520fu-shell.service ]] && ! grep -q "/usr/local/bin/tb520fu-shell" /etc/systemd/user/tb520fu-shell.service; then
    cp -a /etc/systemd/user/tb520fu-shell.service /etc/systemd/user/tb520fu-shell.service.quickshell-bak
fi
install -Dm644 "${HERE}/systemd/system/tb520fu-daemon.service" /etc/systemd/system/tb520fu-daemon.service
install -Dm644 "${HERE}/systemd/user/tb520fu-shell.service" /etc/systemd/user/tb520fu-shell.service
install -Dm644 "${HERE}/systemd/user/tb520fu-osk.service" /etc/systemd/user/tb520fu-osk.service
install -Dm644 "${HERE}/systemd/user/tb520fu-idle.service" /etc/systemd/user/tb520fu-idle.service

for legacy_sys in tb520fu-power-key.service tb520fu-pen-charge.service; do
    systemctl disable --now "$legacy_sys" 2>/dev/null || true
done
rm -f /etc/systemd/user/default.target.wants/tb520fu-mute-led.service
run_user_systemctl disable --now tb520fu-mute-led.service squeekboard.service
pkill -u "$TARGET_USER" -x quickshell 2>/dev/null || true
pkill -u "$TARGET_USER" -x squeekboard 2>/dev/null || true

systemctl daemon-reload
systemctl enable tb520fu-daemon.service
systemctl restart tb520fu-daemon.service

echo "==> [5/7] Installing Niri config, fcitx5 Pinyin profile & ~/Pictures/Standby for ${TARGET_USER}..."
NIRI_DIR="${TARGET_HOME}/.config/niri"
install -d -m755 -o "$TARGET_USER" -g "$TARGET_USER" \
    "$NIRI_DIR" \
    "${TARGET_HOME}/Pictures/Screenshots" \
    "${TARGET_HOME}/Pictures/Standby"
if [[ -f "${NIRI_DIR}/config.kdl" && ! -f "${NIRI_DIR}/config.kdl.pre-rust" ]]; then
    cp -a "${NIRI_DIR}/config.kdl" "${NIRI_DIR}/config.kdl.pre-rust"
fi
install -m644 -o "$TARGET_USER" -g "$TARGET_USER" "${HERE}/niri/config-rust-desktop.kdl" "${NIRI_DIR}/config.kdl"

# Ensure foot terminal uses crisp 11.5pt font (`dpi-aware=no`) + inner padding (`pad=8x6`) for 2.0x HiDPI
FOOT_DIR="${TARGET_HOME}/.config/foot"
install -d -m755 -o "$TARGET_USER" -g "$TARGET_USER" "$FOOT_DIR"
if [[ ! -f "${FOOT_DIR}/foot.ini" ]]; then
    cat > "${FOOT_DIR}/foot.ini" <<'EOF'
[main]
font=DejaVu Sans Mono:size=11.5, Noto Sans Mono CJK SC:size=11.5, Noto Color Emoji:size=11.5
dpi-aware=no
pad=8x6
EOF
    chown "${TARGET_USER}:${TARGET_USER}" "${FOOT_DIR}/foot.ini"
elif ! grep -q '^font=' "${FOOT_DIR}/foot.ini"; then
    cat > "${FOOT_DIR}/foot.ini" <<'EOF'
[main]
font=DejaVu Sans Mono:size=11.5, Noto Sans Mono CJK SC:size=11.5, Noto Color Emoji:size=11.5
dpi-aware=no
pad=8x6
EOF
    chown "${TARGET_USER}:${TARGET_USER}" "${FOOT_DIR}/foot.ini"
fi

# Ensure mpv uses Wayland Vulkan gpu-next + HDR target-colorspace-hint + V4L2 hardware decode by default
MPV_DIR="${TARGET_HOME}/.config/mpv"
install -d -m755 -o "$TARGET_USER" -g "$TARGET_USER" "$MPV_DIR"
if [[ ! -f "${MPV_DIR}/mpv.conf" ]] || ! grep -q '^target-colorspace-hint=' "${MPV_DIR}/mpv.conf"; then
    cat > "${MPV_DIR}/mpv.conf" <<'EOF'
vo=gpu-next
gpu-context=waylandvk
target-colorspace-hint=yes
hwdec=v4l2m2m-copy
EOF
    chown "${TARGET_USER}:${TARGET_USER}" "${MPV_DIR}/mpv.conf"
fi

# Ensure default fcitx5 profile includes English + Chinese Pinyin out of the box
FCITX_DIR="${TARGET_HOME}/.config/fcitx5"
if [[ ! -f "${FCITX_DIR}/profile" ]]; then
    install -d -m755 -o "$TARGET_USER" -g "$TARGET_USER" "$FCITX_DIR"
    cat > "${FCITX_DIR}/profile" <<'EOF'
[Groups/0]
Name=Default
Default Layout=us
DefaultIM=pinyin

[Groups/0/Items/0]
Name=keyboard-us
Layout=

[Groups/0/Items/1]
Name=pinyin
Layout=

[GroupOrder]
0=Default
EOF
    chown "${TARGET_USER}:${TARGET_USER}" "${FCITX_DIR}/profile"
fi

echo "==> [6/7] Optional fcitx5 direct-commit addon (liby700osk.so)..."
if [[ "$WITH_ADDON" -eq 1 || -d /usr/include/Fcitx5/Core ]]; then
    if [[ ! -d /usr/include/Fcitx5/Core ]] || ! command -v make >/dev/null 2>&1; then
        DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
            cmake make g++ pkg-config fcitx5-modules-dev libfcitx5core-dev libfcitx5utils-dev
    fi
    TMP_BUILD="$(mktemp -d)"
    cmake -B "$TMP_BUILD" -S "${HERE}/fcitx5-addon" -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX=/usr
    cmake --build "$TMP_BUILD" -j"$(nproc)"
    cmake --install "$TMP_BUILD"
    rm -rf "$TMP_BUILD"
    echo "    Built and installed liby700osk.so"
else
    echo "    Skipped C++ addon build (pass --with-fcitx-addon to build)"
fi

echo "==> [7/7] Enabling & starting user session services..."
sudo -u "$TARGET_USER" mkdir -p \
    "${TARGET_HOME}/.config/systemd/user/niri.service.wants" \
    "${TARGET_HOME}/.config/systemd/user/graphical-session.target.wants"
for u in tb520fu-shell.service tb520fu-osk.service tb520fu-idle.service; do
    ln -sf "/etc/systemd/user/${u}" "${TARGET_HOME}/.config/systemd/user/niri.service.wants/${u}"
    ln -sf "/etc/systemd/user/${u}" "${TARGET_HOME}/.config/systemd/user/graphical-session.target.wants/${u}"
done
chown -h "${TARGET_USER}:${TARGET_USER}" \
    "${TARGET_HOME}/.config/systemd/user/niri.service.wants/"tb520fu-*.service \
    "${TARGET_HOME}/.config/systemd/user/graphical-session.target.wants/"tb520fu-*.service

run_user_systemctl daemon-reload
run_user_systemctl restart tb520fu-shell.service tb520fu-osk.service tb520fu-idle.service
if [[ -d "/run/user/${TARGET_UID}" ]] && command -v fcitx5 >/dev/null 2>&1; then
    # Reload Niri config & start fcitx5 in live session
    sudo -u "$TARGET_USER" XDG_RUNTIME_DIR="/run/user/${TARGET_UID}" \
        sh -c 'NIRI_SOCKET=$(ls /run/user/'"${TARGET_UID}"'/niri.*.sock 2>/dev/null | head -n1) /usr/local/bin/niri msg action load-config-file >/dev/null 2>&1 || true; nohup fcitx5 -d --replace >/dev/null 2>&1 &' || true
fi

echo ""
echo "==> TB520FU Pure-Rust Desktop Suite + PAM Login/Lock + Standby Companion deployed!"
systemctl --no-pager --full status tb520fu-daemon.service | head -n 12 || true

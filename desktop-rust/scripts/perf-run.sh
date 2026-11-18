#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
# 上机测帧时间（docs/REWORK.md「怎么测」）。
#
#   scripts/perf-run.sh <标签>                          # 默认：只统计，不同步等 GPU
#   EXTRA_ENV=TB520FU_PERF_SYNC=1 scripts/perf-run.sh x  # 每帧提交后等 GPU，得到 gpu_time[id]（会改变节奏，只用于诊断）
#   scripts/perf-run.sh x | python3 -I scripts/perf-agg.py   # 汇总成一张表
#
# 做的事：把本机刚编出来的 target/.../tb520fu-shell 拷到平板 ~/tb520fu-shell.perf，
# 若会话锁着先解锁（锁屏时下拉面板根本不显示，测不到），停掉正式外壳服务，
# 用 systemd-run 临时单元带 TB520FU_PERF=1 跑测试版，开合控制台 5 次、开一个 foot 再开合一次并 grim 截两张图，
# 然后停测试版、启动正式服务、按原样重新锁屏，最后把测试版的 journal 打出来。
# 截图存到 $OUT（默认 target/perf-shots/，target/ 不进 git）。
# GOV=xxx 可临时换 GPU devfreq 调频器；这台只有 userspace / simple_ondemand，没有 performance。
set -euo pipefail
LABEL="${1:-run}"
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${OUT:-$REPO/target/perf-shots}"
mkdir -p "$OUT"
O=(-i "$HOME/.ssh/tb520fu_debug" -o ConnectTimeout=6 -o BatchMode=yes -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR)
scp "${O[@]}" "$REPO/target/aarch64-unknown-linux-gnu/release/tb520fu-shell" debian@yoga-tb520fu.local:/home/debian/tb520fu-shell.perf
timeout 110 ssh "${O[@]}" debian@yoga-tb520fu.local LABEL="$LABEL" EXTRA_ENV="${EXTRA_ENV:-}" GOV="${GOV:-}" bash -s <<'REMOTE'
export XDG_RUNTIME_DIR=/run/user/1000 DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus
export NIRI_SOCKET=$(ls /run/user/1000/niri.*.sock | head -1)
export WAYLAND_DISPLAY=wayland-1
S=$(loginctl list-sessions --no-legend | awk '$3=="debian" && /seat0/ {print $1; exit}')
WAS_LOCKED=$(loginctl show-session "$S" -p LockedHint --value)
U=tb520fu-shell-perf-$LABEL-$$
if [ "$WAS_LOCKED" = yes ]; then /usr/local/bin/tb520fu-shell unlock; sleep 2.5; fi
GPU=/sys/class/devfreq/3d00000.gpu
OLDGOV=$(cat $GPU/governor)
[ -n "$GOV" ] && echo "$GOV" | sudo -n tee $GPU/governor >/dev/null
systemctl --user stop tb520fu-shell.service
systemd-run --user --quiet --unit=$U -E TB520FU_PERF=1 ${EXTRA_ENV:+-E $EXTRA_ENV} /home/debian/tb520fu-shell.perf
sleep 3
for i in 1 2 3 4; do /home/debian/tb520fu-shell.perf console; sleep 1.6; /home/debian/tb520fu-shell.perf console; sleep 1.4; done
niri msg action spawn -- foot -e sh -c 'ls --color=always -la /usr/bin | head -120; sleep 9' ; sleep 1.5
/home/debian/tb520fu-shell.perf console; sleep 1.5
grim /tmp/shot-open.png 2>/dev/null; sleep 0.3
/home/debian/tb520fu-shell.perf console; sleep 1.2
grim /tmp/shot-closed.png 2>/dev/null
echo "gpu governor during run: $(cat $GPU/governor) cur_freq=$(cat $GPU/cur_freq)"
[ -n "$GOV" ] && echo "$OLDGOV" | sudo -n tee $GPU/governor >/dev/null
echo "gpu governor restored: $(cat $GPU/governor)"
systemctl --user is-active $U.service || echo "!!! test shell died"
systemctl --user stop $U.service 2>/dev/null
systemctl --user start tb520fu-shell.service; sleep 2
if [ "$WAS_LOCKED" = yes ]; then /usr/local/bin/tb520fu-shell lock; sleep 1; fi
echo "restored: shell=$(systemctl --user is-active tb520fu-shell.service) locked=$(loginctl show-session "$S" -p LockedHint --value) (was $WAS_LOCKED)"
journalctl --user -u $U.service --no-pager -o cat --since "-70s" | grep -v -E '^(Started|Stopp)'
REMOTE
scp "${O[@]}" debian@yoga-tb520fu.local:/tmp/shot-open.png debian@yoga-tb520fu.local:/tmp/shot-closed.png "$OUT/" 2>/dev/null \
  && for f in open closed; do mv "$OUT/shot-$f.png" "$OUT/$LABEL-$f.png"; done \
  && echo "截图：$OUT/$LABEL-open.png  $OUT/$LABEL-closed.png" >&2

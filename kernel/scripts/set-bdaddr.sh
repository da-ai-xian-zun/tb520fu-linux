#!/bin/sh
# SPDX-License-Identifier: MIT
# Write this tablet's factory Bluetooth address into the compiled device tree.
#
#   set-bdaddr.sh <path/to/sm8650-lenovo-tb520fu.dtb>
#
# The device tree source carries a placeholder (all zeros): the address is unique to
# each tablet and is not committed. Run this after `make qcom/sm8650-lenovo-tb520fu.dtb`
# and before `make Image`, so the embedded DTB carries the address.
#
# The address comes from $TB520FU_BT_ADDR, or BT_ADDR= in device.local.conf, looked up
# next to the kernel tree's parent directory ($W) and at the root of this repository
# (see device.conf.example). It is not set at boot with `btmgmt public-addr` because
# that races with bluetoothd and the controller drops off the management interface.
#
# Without an address the build fails, so a kernel for this tablet never ships the
# placeholder by accident; TB520FU_BT_ADDR=none keeps the placeholder deliberately.
set -eu
DTB=$1
NODE=/soc@0/geniqup@8c0000/serial@898000/bluetooth
HERE=$(dirname "$(realpath "$0")")

addr=${TB520FU_BT_ADDR:-}
if [ -z "$addr" ]; then
	for conf in "${W:-/nonexistent}/device.local.conf" "$HERE/../../device.local.conf"; do
		[ -r "$conf" ] || continue
		addr=$(sed -n 's/^BT_ADDR=//p' "$conf" | tr -d ' \r')
		[ -n "$addr" ] && break
	done
fi

if [ "$addr" = none ]; then
	echo "set-bdaddr: keeping the placeholder Bluetooth address" >&2
	exit 0
fi
if ! echo "$addr" | grep -Eq '^([0-9A-Fa-f]{2}:){5}[0-9A-Fa-f]{2}$'; then
	echo "set-bdaddr: no Bluetooth address (set TB520FU_BT_ADDR or BT_ADDR in device.local.conf," >&2
	echo "            or TB520FU_BT_ADDR=none to keep the placeholder)" >&2
	exit 1
fi

# local-bd-address is little-endian: reverse the bytes.
bytes=$(echo "$addr" | tr ':' '\n' | sed '1!G;h;$!d' | tr '\n' ' ')
# shellcheck disable=SC2086
fdtput -t bx "$DTB" "$NODE" local-bd-address $bytes
echo "set-bdaddr: local-bd-address = $(fdtget -t bx "$DTB" "$NODE" local-bd-address)" >&2

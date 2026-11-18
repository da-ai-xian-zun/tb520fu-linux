#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Sync BLE bonding keys (LTK / IRK) from Android `/data/misc/bluedroid/bt_config.conf`
into Linux BlueZ `/var/lib/bluetooth/<adapter>/<device>/info` so Android & Linux dual-boot
share the exact same BLE pairing (e.g. the Lenovo Tab Pen Pro and BLE earbuds).
Both systems must use the same Bluetooth address (see device.conf.example); the adapter
is taken from the [Adapter] Address in bt_config.conf, else the only one BlueZ knows.

Usage:
  1. In Android (via root adb):
       adb shell su -c 'cat /data/misc/bluedroid/bt_config.conf' > /tmp/bt_config.conf
  2. Copy `/tmp/bt_config.conf` to Debian and run as root:
       sudo python3 bt-sync-android-keys.py /tmp/bt_config.conf [MAC ...]
"""

import os
import re
import shutil
import subprocess
import sys

BLUEZ_DIR = "/var/lib/bluetooth"
MAC_RE = re.compile(r"^([0-9A-F]{2}:){5}[0-9A-F]{2}$")


def adapter_dir(cfg: dict[str, dict[str, str]]) -> str:
    """BlueZ storage directory of the adapter these keys belong to."""
    addr = cfg.get("ADAPTER", {}).get("Address", "").upper()
    if MAC_RE.match(addr) and os.path.isdir(os.path.join(BLUEZ_DIR, addr)):
        return os.path.join(BLUEZ_DIR, addr)
    known = [d for d in os.listdir(BLUEZ_DIR) if MAC_RE.match(d)]
    if len(known) == 1:
        if addr and addr != known[0]:
            print(f"warning: Android adapter {addr} differs from BlueZ adapter {known[0]};"
                  " the pairings will not match", file=sys.stderr)
        return os.path.join(BLUEZ_DIR, known[0])
    raise SystemExit(f"cannot tell which adapter in {BLUEZ_DIR} to use: {known}")


def parse_bt_config(path: str) -> dict[str, dict[str, str]]:
    sections: dict[str, dict[str, str]] = {}
    cur: str | None = None
    with open(path, "r", encoding="utf-8", errors="replace") as f:
        for raw in f:
            line = raw.strip()
            if not line or line.startswith("#") or line.startswith(";"):
                continue
            if line.startswith("[") and line.endswith("]"):
                cur = line[1:-1].strip().upper()
                sections.setdefault(cur, {})
            elif cur is not None and "=" in line:
                k, v = line.split("=", 1)
                sections[cur][k.strip()] = v.strip()
    return sections


def update_bluez_info(info_path: str, props: dict[str, str]) -> bool:
    penc_hex = props.get("LE_KEY_PENC", "")
    if len(penc_hex) < 56:
        return False
    penc = bytes.fromhex(penc_hex[:56])
    ltk_hex = penc[0:16].hex().upper()
    rand_u64 = int.from_bytes(penc[16:24], "little")
    ediv_u16 = int.from_bytes(penc[24:26], "little")
    sec_level = penc[26]
    key_size = penc[27]
    auth = 1 if sec_level >= 2 else 0

    pid_hex = props.get("LE_KEY_PID", "")
    irk_hex = bytes.fromhex(pid_hex[:32]).hex().upper() if len(pid_hex) >= 32 else None

    lenc_hex = props.get("LE_KEY_LENC", "")
    lltk_hex = bytes.fromhex(lenc_hex[:32]).hex().upper() if len(lenc_hex) >= 32 else None

    shutil.copy2(info_path, info_path + ".bak")
    with open(info_path, "r", encoding="utf-8") as f:
        lines = f.read().splitlines()

    out: list[str] = []
    sec = ""
    has_ltk = False
    for line in lines:
        if line.startswith("["):
            sec = line.strip()
            out.append(line)
            if sec == "[LongTermKey]":
                has_ltk = True
        elif sec == "[LongTermKey]":
            if line.startswith("Key="):
                out.append(f"Key={ltk_hex}")
            elif line.startswith("Authenticated="):
                out.append(f"Authenticated={auth}")
            elif line.startswith("EncSize="):
                out.append(f"EncSize={key_size}")
            elif line.startswith("EDiv="):
                out.append(f"EDiv={ediv_u16}")
            elif line.startswith("Rand="):
                out.append(f"Rand={rand_u64}")
            else:
                out.append(line)
        elif sec == "[IdentityResolvingKey]" and irk_hex and line.startswith("Key="):
            out.append(f"Key={irk_hex}")
        elif sec in ("[PeripheralLongTermKey]", "[SlaveLongTermKey]") and lltk_hex and line.startswith("Key="):
            out.append(f"Key={lltk_hex}")
        else:
            out.append(line)

    if not has_ltk:
        out.extend([
            "",
            "[LongTermKey]",
            f"Key={ltk_hex}",
            f"Authenticated={auth}",
            f"EncSize={key_size}",
            f"EDiv={ediv_u16}",
            f"Rand={rand_u64}",
        ])

    with open(info_path, "w", encoding="utf-8") as f:
        f.write("\n".join(out) + "\n")
    os.chmod(info_path, 0o600)
    return True


def main() -> int:
    if len(sys.argv) < 2:
        print(__doc__.strip(), file=sys.stderr)
        return 1
    cfg = parse_bt_config(sys.argv[1])
    targets = {m.upper() for m in sys.argv[2:]}
    adapter = adapter_dir(cfg)

    matched: list[str] = []
    for mac, props in cfg.items():
        if ":" not in mac or (targets and mac not in targets):
            continue
        info_path = os.path.join(adapter, mac, "info")
        if os.path.exists(info_path) and "LE_KEY_PENC" in props:
            matched.append(mac)

    if not matched:
        print("No matching paired LE devices with existing BlueZ info found.", file=sys.stderr)
        return 1

    subprocess.run(["systemctl", "stop", "bluetooth"], check=True)
    try:
        for mac in matched:
            info_path = os.path.join(adapter, mac, "info")
            if update_bluez_info(info_path, cfg[mac]):
                print(f"Synced BLE keys for {mac} ({cfg[mac].get('Name', '?')})")
    finally:
        subprocess.run(["systemctl", "start", "bluetooth"], check=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

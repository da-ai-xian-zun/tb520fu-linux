#!/bin/bash
# SPDX-License-Identifier: GPL-2.0
# build_initramfs.sh - Assemble initramfs.cpio for CONFIG_INITRAMFS_SOURCE
# Includes static ARM64 busybox (/bin/busybox), static ARM64 dmsetup (/sbin/dmsetup),
# /dev/console (c 5 1) + /dev/null (c 1 3) device nodes, and /etc/tb520fu_stage.
#
# Usage:
#   ./build_initramfs.sh [--stage stage1|stage2|stage3] [path/to/mainline_dm_table.txt] [path/to/mainline_root_uuid.txt]
#
# Stage defaults (per REVIEW-20261108.md Section 7 N4):
#   - When no dm_table is provided: defaults to "stage1" (nodm; userdata untouched).
#   - When dm_table + uuid are provided: defaults to "stage2" (ro_verify_only;
#     verifies 64 KiB + 64 MiB btrfs superblocks & UUID and mounts @main read-only).
#   - Pass "--stage stage3" explicitly when ready for normal read-write switch_root.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REQ_STAGE=""

while [ $# -gt 0 ]; do
    case "$1" in
        --stage)
            REQ_STAGE="${2:-}"
            shift 2
            ;;
        --stage=*)
            REQ_STAGE="${1#*=}"
            shift
            ;;
        *)
            break
            ;;
    esac
done

DM_TABLE="${1:-${SCRIPT_DIR}/etc/mainline_dm_table.txt}"
UUID_FILE="${2:-${SCRIPT_DIR}/etc/mainline_root_uuid.txt}"
OUT_CPIO="${SCRIPT_DIR}/initramfs.cpio"

[ -x "${SCRIPT_DIR}/init" ] || { echo "ERROR: ${SCRIPT_DIR}/init missing"; exit 1; }
[ -x "${SCRIPT_DIR}/bin/busybox" ] || { echo "ERROR: ${SCRIPT_DIR}/bin/busybox missing"; exit 1; }
[ -x "${SCRIPT_DIR}/sbin/dmsetup" ] || { echo "ERROR: ${SCRIPT_DIR}/sbin/dmsetup missing"; exit 1; }

WORK_ROOT="$(mktemp -d)"
trap 'rm -rf "$WORK_ROOT"' EXIT

mkdir -p "$WORK_ROOT/"{bin,sbin,etc,proc,sys,dev,newroot,run}
cp -f "${SCRIPT_DIR}/init" "$WORK_ROOT/init"
chmod 0755 "$WORK_ROOT/init"

cp -f "${SCRIPT_DIR}/bin/busybox" "$WORK_ROOT/bin/busybox"
chmod 0755 "$WORK_ROOT/bin/busybox"
ln -sf busybox "$WORK_ROOT/bin/sh"

cp -f "${SCRIPT_DIR}/sbin/dmsetup" "$WORK_ROOT/sbin/dmsetup"
chmod 0755 "$WORK_ROOT/sbin/dmsetup"

if [ -f "$DM_TABLE" ] && [ -s "$DM_TABLE" ]; then
    if [ ! -f "$UUID_FILE" ] || [ ! -s "$UUID_FILE" ]; then
        echo "ERROR: $DM_TABLE provided without matching $UUID_FILE!" >&2
        exit 1
    fi
    cp -f "$DM_TABLE" "$WORK_ROOT/etc/mainline_dm_table.txt"
    cp -f "$UUID_FILE" "$WORK_ROOT/etc/mainline_root_uuid.txt"
    STAGE="${REQ_STAGE:-stage2}"
    echo "$STAGE" > "$WORK_ROOT/etc/tb520fu_stage"
    echo "[initramfs] Embedded dm-linear table ($DM_TABLE), btrfs UUID ($UUID_FILE), and stage=${STAGE}."
else
    STAGE="${REQ_STAGE:-stage1}"
    echo "$STAGE" > "$WORK_ROOT/etc/tb520fu_stage"
    echo "[initramfs] Built Stage-1 bringup initramfs (stage=${STAGE}, no dm-linear table embedded; userdata untouched)."
fi

python3 - "$WORK_ROOT" "$OUT_CPIO" <<'PY'
import os
import stat
import sys

work_root = sys.argv[1]
out_cpio = sys.argv[2]

def align4(n):
    return (4 - (n % 4)) % 4

def write_newc_entry(f, name, ino, mode, uid, gid, nlink, mtime, filesize, devmaj, devmin, rdevmaj, rdevmin, data=b""):
    name_bytes = name.encode("utf-8") + b"\x00"
    hdr = (
        "070701"
        f"{ino:08x}"
        f"{mode:08x}"
        f"{uid:08x}"
        f"{gid:08x}"
        f"{nlink:08x}"
        f"{mtime:08x}"
        f"{filesize:08x}"
        f"{devmaj:08x}"
        f"{devmin:08x}"
        f"{rdevmaj:08x}"
        f"{rdevmin:08x}"
        f"{len(name_bytes):08x}"
        f"{0:08x}"
    ).encode("ascii")
    f.write(hdr)
    f.write(name_bytes)
    f.write(b"\x00" * align4(len(hdr) + len(name_bytes)))
    if data:
        f.write(data)
        f.write(b"\x00" * align4(len(data)))

ino = 300000
with open(out_cpio, "wb") as out:
    entries = []
    for root, dirs, files in os.walk(work_root):
        dirs.sort()
        files.sort()
        rel_root = os.path.relpath(root, work_root)
        entries.append((root, "." if rel_root == "." else rel_root))
        for fn in files:
            full = os.path.join(root, fn)
            rel = fn if rel_root == "." else os.path.join(rel_root, fn)
            entries.append((full, rel))

    for full, rel in entries:
        st = os.lstat(full)
        ino += 1
        if stat.S_ISLNK(st.st_mode):
            target = os.readlink(full).encode("utf-8")
            write_newc_entry(out, rel, ino, st.st_mode, 0, 0, 1, int(st.st_mtime), len(target), 0, 0, 0, 0, target)
        elif stat.S_ISDIR(st.st_mode):
            write_newc_entry(out, rel, ino, st.st_mode, 0, 0, 2, int(st.st_mtime), 0, 0, 0, 0, 0)
        elif stat.S_ISREG(st.st_mode):
            with open(full, "rb") as rf:
                data = rf.read()
            write_newc_entry(out, rel, ino, st.st_mode, 0, 0, 1, int(st.st_mtime), len(data), 0, 0, 0, 0, data)

    # Inject /dev/console (c 5 1) and /dev/null (c 1 3) directly into cpio (N1)
    ino += 1
    write_newc_entry(out, "dev/console", ino, stat.S_IFCHR | 0o600, 0, 0, 1, 0, 0, 0, 0, 5, 1)
    ino += 1
    write_newc_entry(out, "dev/null", ino, stat.S_IFCHR | 0o666, 0, 0, 1, 0, 0, 0, 0, 1, 3)

    # Trailer
    write_newc_entry(out, "TRAILER!!!", 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0)
PY

echo "[OK] Generated $OUT_CPIO ($(stat -c %s "$OUT_CPIO") bytes)"

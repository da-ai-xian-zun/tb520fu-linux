#!/bin/bash
# SPDX-License-Identifier: MIT
# Build boot_uboot_linux.img: U-Boot with the current Android kernel 8 MiB in and
# a gzipped mainline Linux after it, packed and signed like the boot_a image
# running on the tablet.
# Nothing here touches the tablet.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SRC="$HERE/u-boot"
PORT="$HERE/uboot-port"
GKI=${TB520FU_GKI:-$HOME/tb520fu-gki-r13}   # AOSP GKI checkout: mkbootimg, avbtool
MKBOOTIMG="$GKI/tools/mkbootimg/mkbootimg.py"
AVBTOOL="$GKI/prebuilts/kernel-build-tools/linux-x86/bin/avbtool"
KEY="${TB520FU_AVB_KEY:-$HERE/../../../设备/pixel8/keys/testkey_rsa4096.pem}"   # AOSP external/avb/test/data/testkey_rsa4096.pem
BOOT_SIZE=100663296
RI=1762300800
PAYLOAD=$((0x800000))	# must match android_addr in tb520fu.env (0xa8000000 + this)
GZ_OFFSET=$((0x3400000))	# must match linux_gz_addr in tb520fu.env (0xa8000000 + this)
LINUX_ADDR=$((0xae000000))	# must match linux_addr in tb520fu.env
ABL_FDT=$((0xb72d1000))		# observed on the tablet; unpacked Linux must stay below it
LINUX_GZ="${LINUX_GZ:-$HERE/../tb520fu-mainline-linux/out/7.3.3-tb520fu-lapis/Image.gz}"
OUT="$HERE/out"
[ -f "$OUT/android.Image" ] || { echo "run build.sh first, it extracts out/android.Image"; exit 1; }

cp "$PORT/tb520fu.config" "$PORT/tb520fu.env" "$SRC/board/qualcomm/"
cp "$PORT/androidboot.c" "$PORT/bootflag.c" "$SRC/arch/arm/mach-snapdragon/"
cp "$PORT/sm8650-lenovo-tb520fu-u-boot.dtsi" "$SRC/arch/arm/dts/"
cp "$PORT/sm8650-lenovo-tb520fu.dts" "$SRC/dts/upstream/src/arm64/qcom/"
export CROSS_COMPILE=aarch64-linux-gnu-
make -C "$SRC" O=.output qcom_defconfig tb520fu.config </dev/null >/dev/null
make -C "$SRC" O=.output -j"$(nproc)" 2>&1 | grep -E 'error|Error|undefined' || true
UBOOT="$SRC/.output/u-boot.bin"
[ "$(stat -c %s "$UBOOT")" -lt "$PAYLOAD" ]
grep -q '^CONFIG_TB520FU_BOOTFLAG=y' "$SRC/.output/.config" || { echo "bootflag 没打开"; exit 1; }

# U-Boot's own header carries its link-time size and a nonsense offset. Give ABL
# the same header values the working stage 0 image had.
ANDROID_MEMSZ=$(python3 -I -c 'import struct,sys;print(struct.unpack("<Q",open(sys.argv[1],"rb").read(24)[16:24])[0])' "$OUT/android.Image")
[ $((PAYLOAD + ANDROID_MEMSZ)) -le "$GZ_OFFSET" ] || { echo "Android kernel overlaps Linux payload"; exit 1; }
LINUX_MEMSZ=$(python3 -I -c 'import gzip,struct,sys;print(struct.unpack("<Q",gzip.open(sys.argv[1],"rb").read(24)[16:24])[0])' "$LINUX_GZ")
[ $((LINUX_ADDR + LINUX_MEMSZ)) -lt "$ABL_FDT" ] || { echo "unpacked Linux would overwrite the devicetree from ABL"; exit 1; }
[ $((0xa8000000 + GZ_OFFSET + $(stat -c %s "$LINUX_GZ"))) -le "$LINUX_ADDR" ] || { echo "Linux Image.gz overlaps its unpack address"; exit 1; }
cp "$UBOOT" "$OUT/combined.Image"
truncate -s "$PAYLOAD" "$OUT/combined.Image"
cat "$OUT/android.Image" >> "$OUT/combined.Image"
truncate -s "$GZ_OFFSET" "$OUT/combined.Image"
cat "$LINUX_GZ" >> "$OUT/combined.Image"
python3 -I - "$OUT/combined.Image" "$(stat -c %s "$OUT/combined.Image")" <<'PY'
import struct, sys
with open(sys.argv[1], 'r+b') as f:
    h = f.read(64)
    assert h[56:60] == b'ARM\x64', 'not an arm64 Image'
    f.seek(8)
    f.write(struct.pack('<QQQ', 0, int(sys.argv[2]), 0xa))
PY

IMG="$OUT/${IMG_NAME:-boot_uboot_linux.img}"
python3 "$MKBOOTIMG" --header_version 4 --kernel "$OUT/combined.Image" \
	--cmdline "max_loop=64 log_buf_len=16M" --output "$IMG"
"$AVBTOOL" add_hash_footer --image "$IMG" --partition_name boot \
	--partition_size "$BOOT_SIZE" --key "$KEY" --algorithm SHA256_RSA4096 \
	--rollback_index "$RI" \
	--prop com.android.build.boot.os_version:14 \
	--prop "com.android.build.boot.fingerprint:Lenovo/TB520FU/TB520FU:14/UKQ1.240826.001/ZUI_18.5.10.037_260929_ROW:user/release-keys" \
	--prop com.android.build.boot.security_patch:2026-08-05

# verify_image reads the hash-descriptor partition as <dir>/<partition>.img
ln -sf "$(basename "$IMG")" "$OUT/boot.img"
"$AVBTOOL" verify_image --image "$OUT/boot.img" --key "$KEY"
echo "size   $(stat -c %s "$IMG") (expected $BOOT_SIZE)"
echo "sha256 $(sha256sum "$IMG" | cut -c1-64)"

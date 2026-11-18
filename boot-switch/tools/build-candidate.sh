#!/bin/bash
# SPDX-License-Identifier: MIT
# 造“支持从 Linux 重启进安卓”的候选 boot_a 镜像。
#
# 做法：从一个已经在用的 boot 镜像里取出内核载荷，只把开头 8 MiB 的 U-Boot 换成
# 带 bootflag 的版本，其余（安卓内核、Linux Image.gz）一字节不动，再按原样重新签名。
# 这样候选镜像和在机镜像只差 U-Boot，内核和模块不用跟着换。
#
# 用法：build-candidate.sh [基准镜像] [输出镜像]
#   默认基准 out/boot_uboot_linux_full4.img，默认输出 out/boot_uboot_linux_bootflag.img
# 这个脚本不碰平板，也不改 build_uboot.sh 的默认构建（U-Boot 用单独的输出目录）。
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SRC="$HERE/u-boot"
PORT="$HERE/uboot-port"
OUT="$HERE/out"
BASE="${1:-$OUT/boot_uboot_linux_full4.img}"
IMG="${2:-$OUT/boot_uboot_linux_bootflag.img}"
GKI=${TB520FU_GKI:-$HOME/tb520fu-gki-r13}   # AOSP GKI checkout: mkbootimg, avbtool
MKBOOTIMG="$GKI/tools/mkbootimg/mkbootimg.py"
AVBTOOL="$GKI/prebuilts/kernel-build-tools/linux-x86/bin/avbtool"
KEY="${TB520FU_AVB_KEY:-$HERE/../../../设备/pixel8/keys/testkey_rsa4096.pem}"   # AOSP external/avb/test/data/testkey_rsa4096.pem
# 下面几项必须和 build_uboot.sh 一致；末尾会拿基准镜像的 AVB 信息逐项比对。
BOOT_SIZE=100663296
RI=1762300800
PAYLOAD=$((0x800000))
WORK="$OUT/bootflag-work"
mkdir -p "$WORK"

[ -f "$BASE" ] || { echo "找不到基准镜像 $BASE"; exit 1; }

# 1. U-Boot：单独的输出目录（bootflag 已合入默认 tb520fu.config / tb520fu.env）。
cp "$PORT/tb520fu.config" "$PORT/tb520fu.env" "$SRC/board/qualcomm/"
cp "$PORT/androidboot.c" "$PORT/bootflag.c" "$SRC/arch/arm/mach-snapdragon/"
cp "$PORT/sm8650-lenovo-tb520fu-u-boot.dtsi" "$SRC/arch/arm/dts/"
cp "$PORT/sm8650-lenovo-tb520fu.dts" "$SRC/dts/upstream/src/arm64/qcom/"
export CROSS_COMPILE=aarch64-linux-gnu-
make -C "$SRC" O=.output-bootflag qcom_defconfig tb520fu.config </dev/null >/dev/null
make -C "$SRC" O=.output-bootflag -j"$(nproc)" 2>&1 | grep -E 'error|Error|undefined|warning' || true
UBOOT="$SRC/.output-bootflag/u-boot.bin"
[ -f "$UBOOT" ] || { echo "U-Boot 没编出来"; exit 1; }
[ "$(stat -c %s "$UBOOT")" -lt "$PAYLOAD" ]
grep -q '^CONFIG_TB520FU_BOOTFLAG=y' "$SRC/.output-bootflag/.config" || { echo "bootflag 没打开"; exit 1; }

# 2. 取出基准镜像的内核载荷，换掉开头的 U-Boot。
python3 -I - "$BASE" "$UBOOT" "$WORK/combined.Image" "$PAYLOAD" <<'PY'
import struct, sys
base, uboot, out, payload = sys.argv[1], sys.argv[2], sys.argv[3], int(sys.argv[4])
d = open(base, 'rb').read()
assert d[:8] == b'ANDROID!', '基准不是 boot 镜像'
ksize, rsize, _, hsize = struct.unpack_from('<IIII', d, 8)
hver, = struct.unpack_from('<I', d, 40)
assert hver == 4 and rsize == 0, (hver, rsize)
k = d[4096:4096 + ksize]
assert k[56:60] == b'ARM\x64', '载荷开头不是 arm64 Image'
assert k[payload + 56:payload + 60] == b'ARM\x64', '8 MiB 处不是安卓内核'
u = open(uboot, 'rb').read()
assert u[56:60] == b'ARM\x64' and len(u) < payload
new = bytearray(u.ljust(payload, b'\0') + k[payload:])
# 和 build_uboot.sh 一样改写 arm64 头：text_offset 0、image_size 整个载荷、flags 0xa
struct.pack_into('<QQQ', new, 8, 0, len(new), 0xa)
assert struct.unpack_from('<QQQ', k, 8) == (0, len(k), 0xa), '基准头和预期不同'
assert len(new) == len(k) and new[payload:] == k[payload:]
open(out, 'wb').write(new)
print('载荷 %d 字节，U-Boot %d 字节，其余与基准相同' % (len(new), len(u)))
PY

# 3. 打包、签名，和基准镜像同样的参数。
python3 "$MKBOOTIMG" --header_version 4 --kernel "$WORK/combined.Image" \
	--cmdline "max_loop=64 log_buf_len=16M" --output "$IMG"
"$AVBTOOL" add_hash_footer --image "$IMG" --partition_name boot \
	--partition_size "$BOOT_SIZE" --key "$KEY" --algorithm SHA256_RSA4096 \
	--rollback_index "$RI" \
	--prop com.android.build.boot.os_version:14 \
	--prop "com.android.build.boot.fingerprint:Lenovo/TB520FU/TB520FU:14/UKQ1.240826.001/ZUI_18.5.10.037_260929_ROW:user/release-keys" \
	--prop com.android.build.boot.security_patch:2026-08-05

# 4. 验签，并确认 AVB 信息除了摘要和盐以外和基准一样。
ln -sf "$(realpath "$IMG")" "$WORK/boot.img"
"$AVBTOOL" verify_image --image "$WORK/boot.img" --key "$KEY"
info() { "$AVBTOOL" info_image --image "$1" | grep -vE '^\s*(Salt|Digest):'; }
diff <(info "$BASE") <(info "$IMG") && echo "AVB 信息与基准一致（摘要和盐除外）"
cmp -n 4096 "$BASE" "$IMG" && echo "boot 头与基准一致"
echo "size   $(stat -c %s "$IMG") (expected $BOOT_SIZE)"
echo "sha256 $(sha256sum "$IMG" | cut -c1-64)"
echo "base   $(sha256sum "$BASE" | cut -c1-64)"

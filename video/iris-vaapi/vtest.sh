#!/bin/bash
# SPDX-License-Identifier: MIT
# usage: vtest.sh <driver_dir> <driver_name> <clipdir> [clips...]
D=$1; N=$2; C=$3; shift 3
[ $# -gt 0 ] || set -- test-h264.mp4 test-hevc.mkv test-vp9.webm test-av1.webm
for f in "$@"; do
  b=$(basename "$f"); sw=/tmp/vt-sw.md5; hw=/tmp/vt-hw.md5
  ffmpeg -y -hide_banner -loglevel error -i "$C/$f" -pix_fmt yuv420p -f framemd5 $sw
  LIBVA_DRIVER_NAME=$N LIBVA_DRIVERS_PATH=$D timeout 120 ffmpeg -y -hide_banner -loglevel error \
    -vaapi_device /dev/dri/renderD128 -hwaccel vaapi -hwaccel_output_format vaapi -i "$C/$f" \
    -vf "hwdownload,format=nv12,format=yuv420p" -f framemd5 $hw 2>/tmp/vt-err.txt; rc=$?
  r=$(paste -d" " <(grep -v "^#" $sw | awk -F", *" '{print $6}') <(grep -v "^#" $hw | awk -F", *" '{print $6}') | awk '{n++; if($1==$2)ok++} END{printf "%d/%d", ok, n}')
  echo "$b rc=$rc sw=$(grep -vc '^#' $sw) hw=$(grep -vc '^#' $hw) match=$r $(head -c 160 /tmp/vt-err.txt | tr '\n' ' ')"
done

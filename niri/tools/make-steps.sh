#!/bin/sh
# SPDX-License-Identifier: MIT
# PQ/BT.2020 grey step chart: ~100 nit background, boxes at 203/400/650/1000/4000 nit.
# 8-bit PQ codes: 0x82=100, 0x94=203, 0xA6=400, 0xB3=650, 0xC0=1000, 0xE6=4000 cd/m².
ffmpeg -v error -y -f lavfi -i "color=c=0x828282:s=1920x1080:r=30:d=60,drawbox=x=160:y=390:w=240:h=300:c=0x949494:t=fill,drawbox=x=480:y=390:w=240:h=300:c=0xA6A6A6:t=fill,drawbox=x=800:y=390:w=240:h=300:c=0xB3B3B3:t=fill,drawbox=x=1120:y=390:w=240:h=300:c=0xC0C0C0:t=fill,drawbox=x=1440:y=390:w=240:h=300:c=0xE6E6E6:t=fill,format=yuv420p10le" \
  -c:v libx265 -preset ultrafast -x265-params "log-level=error:colorprim=bt2020:transfer=smpte2084:colormatrix=bt2020nc:max-cll=4000,400" \
  -color_primaries bt2020 -color_trc smpte2084 -colorspace bt2020nc "${1:-steps.mkv}"

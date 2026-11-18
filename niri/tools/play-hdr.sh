#!/bin/sh
# SPDX-License-Identifier: MIT
# Play an HDR10 clip fullscreen so niri sees PQ content (needs Mesa >= 25.1 Vulkan WSI).
exec mpv --vo=gpu-next --gpu-context=waylandvk --target-colorspace-hint=yes --fs --loop "${1:-$HOME/build/niri-hdr/steps.mkv}"

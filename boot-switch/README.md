# 从 Linux 重启进入 Android

原理见 [开机原理](../docs/boot.md#从-linux-重启进入-android)：在 PMIC 的一个寄存器里写入一次性标记后重启，U-Boot 读到标记就清除它，跳过菜单直接启动 Android（或进入 fastboot）。

| 文件 | 作用 |
|---|---|
| `tools/tb520fu-bootflag` | 读写这个一次性标记 |
| `tools/reboot-to-android` | 写入标记并重启进入 Android |
| `tools/build-candidate.sh` | 维护者打包测试用开机镜像的脚本 |

桌面的电源菜单和系统设置里的“进安卓”就是调用这里的功能；[`desktop-rust/install-on-tablet.sh`](../desktop-rust/install-on-tablet.sh) 会把两个工具装到 `/usr/local/sbin/`。U-Boot 一侧的实现在 [`uboot/bootflag.c`](../uboot/bootflag.c)。

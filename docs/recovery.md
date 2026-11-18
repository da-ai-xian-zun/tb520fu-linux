# 出问题怎么办

按从轻到重排列。

## Linux 启动不了

开机菜单里选 `Android`，Android 不受影响。

initramfs 找不到可用的系统时，会先改用 `@daily` 再试一次，仍然失败就进入救援 shell：

- 屏幕上会显示提示信息；
- 用 USB 线连电脑，会出现一个串口设备（通常是 `/dev/ttyACM0`，115200），在电脑上用 `picocom`、`screen` 等打开即可操作；
- 救援 shell 是 root 权限，无需密码。60 秒内没有输入会自动重启；连着电脑时会一直等待。

在救援 shell 里可以挂载 `linux` 分区的 btrfs 顶层，检查子卷，或用 `boot-target.once` 指定下次启动的子卷（见 [开机原理](boot.md#linux-的启动)）。

## 新内核有问题

把之前能用的 `boot_a` 镜像刷回去，并确认平板上还有与它配套的模块目录（见 [从源码构建](building.md#内核和模块必须一起更新)）。

进入 fastboot 的方法：

- 开机菜单选 `Fastboot`；
- 在 Linux 中运行 `systemctl reboot --reboot-argument=bootloader`；
- 在 Android 中运行 `adb reboot bootloader`；
- 关机状态下按住电源键和音量上键。

```bash
fastboot flash boot_a <之前的镜像>
fastboot reboot
```

想彻底回到原厂状态，刷回你备份的原厂 `boot_a` 即可，开机菜单随之消失。

## 连 fastboot 都进不去

`boot_a` 损坏到 ABL 无法启动时，可以用 9008（高通 EDL）模式写回备份的 `boot_a`：

1. 拔掉 USB，长按电源键 10–15 秒强制关机；
2. 按住音量上键，插上 USB，电脑上出现 `05c6:9008` 设备后松开；
3. 用 [qdl](https://github.com/linux-msm/qdl) 和联想官方完整刷机包里的引导程序（`xbl_s_devprg_ns.melf`），把备份的镜像写回 `boot_a`。

写入时需要知道 `boot_a` 在存储上的位置，可以从备份的 GPT 分区表中查到。

## 关于联想官方救砖工具

联想救砖工具的“完整刷机”会：

- 恢复原厂分区表，**清除 `linux` 分区**；
- 把 ABL 等底层固件也刷成刷机包里的版本。如果包里的 ABL 是新版本，之后就无法在锁定状态下启动本项目的镜像。

使用前请确认你了解这些后果，并备份好 `linux` 分区上的数据。

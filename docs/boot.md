# 开机原理

## 总览

```
高通引导链 ──> ABL ──> boot_a 分区
                        ├─ U-Boot（开机菜单）
                        ├─ Android 内核（原样保留）
                        └─ Linux 内核（Image.gz，内含设备树和 initramfs）
```

ABL 受高通安全启动保护，无法修改，也没有开机菜单。`boot_a` 是这台设备上最早能运行自己代码的地方，所以本项目把 U-Boot 放进 `boot_a`，由它显示菜单，再启动 Android 或 Linux。

- 切换系统不会写入任何分区，也不占用 A/B 中的另一个槽。
- 选择 Android 时，U-Boot 把原样保留的 Android 内核和 ABL 准备好的设备树、ramdisk 一起交出去，Android 看到的启动环境与原厂一致。

## 开机菜单

开机后显示 5 秒菜单，用音量键移动，电源键确认：

| 选项 | 作用 |
|---|---|
| `Linux` | 默认选项，5 秒无操作时自动进入 |
| `Android` | 启动 Android |
| `Fastboot` | 重启进入 fastboot |
| `Recovery` | 重启进入 Android Recovery |
| `Reboot` / `Power off` | 重启 / 关机 |

如果 Linux 内核解压失败，菜单会自动转去启动 Android。

U-Boot 不初始化存储和 USB 等外设，尽量不改变 Android 的启动环境。

### 从 Linux 重启进入 Android

桌面的电源菜单和系统设置里有“进安卓”。它在 PMIC 的一个寄存器里写入一次性标记后重启，U-Boot 读到标记会先清除它，然后跳过菜单直接启动 Android。重启进入 fastboot 也用同样的方式。实现见 [`boot-switch/`](../boot-switch/)。

## 验证启动

`boot_a` 必须通过 ABL 的验证启动（AVB）才能运行：

- **签名密钥**：`vbmeta` 中 `boot` 分区的校验公钥是公开的 AOSP 测试密钥，本项目的镜像用这把密钥签名，所以 bootloader 可以保持锁定。
- **ABL 版本**：从 ZUI 17.5.10.362 起（包括 ZUI 18.5），原厂 ABL 不再信任这把密钥，锁定状态下无法启动本项目的镜像。维护者的设备使用的是 17.5.10.096 版本的 ABL。
- **回滚索引**：镜像的回滚索引不能低于设备记录的值，而且这个值只增不减。原厂 17.5.10.362 或更新的版本只要正常启动过，就会把它调高（18.5.10.037 为 `1785888000`）。镜像使用的值在 [`uboot/build_uboot.sh`](../uboot/build_uboot.sh) 的 `RI` 中设置。

镜像格式与原厂 `boot_a` 相同：boot image header v4、没有 ramdisk，分区大小 96 MiB。三部分在镜像中的位置如下（加载基址 `0xa8000000`）：

| 偏移 | 内容 |
|---|---|
| `0` | U-Boot |
| `0x800000` | Android 内核 |
| `0x3400000` | Linux 内核 `Image.gz`，开机时解压到 `0xae000000` |

因此 Linux 的 `Image.gz` 最大约 46 MB。打包脚本会检查各部分不重叠，解压后的内核也不会覆盖 ABL 传来的设备树。

## Linux 的启动

Linux 内核的命令行和设备树都编译在内核里，initramfs 也内嵌在内核中。initramfs 的流程：

1. 找到名为 `linux` 的 GPT 分区，挂载它的 btrfs 顶层。
2. 读取顶层的 `boot-target.once`（读后删除）或 `boot-target`，决定启动哪个子卷，默认 `@daily`。
3. 挂载所选子卷并切换进去。失败时改用 `@daily` 再试一次。
4. 仍然失败就进入救援 shell，在屏幕和 USB 串口上都可以操作。60 秒内没有输入会自动重启；连着电脑时会一直等待。

`linux` 分区上的子卷：

| 子卷 | 用途 |
|---|---|
| `@daily` | 根文件系统 |
| `@home` | 挂载到 `/home` |

想临时从别的子卷启动，在顶层写入 `boot-target.once`，内容为子卷名（例如 `@test`）。

initramfs 的源码在 [`kernel/initramfs/`](../kernel/initramfs/)。

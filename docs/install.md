# 安装概览

> 目前还没有安装程序，也没有逐步的安装教程。本文说明系统由哪些部分组成、各自放在哪里、需要提前准备什么，供熟悉 Android 刷机和 Linux 的用户参考。

## 系统组成

| 部分 | 位置 | 说明 |
|---|---|---|
| 开机镜像 | `boot_a` 分区 | U-Boot 菜单 + Android 内核 + Linux 内核，见 [开机原理](boot.md) |
| Linux 系统 | 新建的 `linux` 分区 | btrfs，子卷 `@daily`（根）和 `@home` |
| 内核模块 | `/usr/lib/modules/<版本>/` | 必须与 `boot_a` 里的内核配套 |
| 固件 | `/usr/lib/firmware/` | 从你自己设备的 Android 中提取，见下文 |
| 硬件配置 | 系统各处 | [`system/`](../system/)、[`audio/`](../audio/)、[`camera/`](../camera/) 等目录 |
| 桌面 | `/usr/local/bin/` 等 | Niri 和 [`desktop-rust/`](../desktop-rust/)，见 [桌面](desktop.md) |

Android 继续使用原来的分区，不受影响（`userdata` 会变小，见下文）。

## 准备

- 一台 ABL 为旧版本的 TB520FU（原因见 [开机原理](boot.md#验证启动)）。
- 一台 Linux 电脑，装好 `adb`、`fastboot` 和交叉编译工具（见 [从源码构建](building.md)）。
- **完整备份**：GPT 分区表，以及 `boot_a`、`init_boot_a`、`vbmeta_a`、`persist`、`fsg` 等每台设备独有的分区。
- 设备专属值：把 [`device.conf.example`](../device.conf.example) 复制为 `device.local.conf`，填入从 Android 中读取的蓝牙地址和 Wi‑Fi MAC。

## 分区

主存储（UFS LUN0）上，`userdata` 是最后一个分区，后面没有空闲空间。需要：

1. 缩小 `userdata`：只改结束位置，保留它的分区类型 GUID 和唯一 GUID；
2. 在空出的空间新建一个分区，**名称必须是 `linux`**（initramfs 按名称查找）；
3. 在 `linux` 分区上建 btrfs，创建子卷 `@daily` 和 `@home`。

**缩小 `userdata` 后，Android 的数据会被清空**，需要重新格式化，相当于恢复出厂设置。

修改 GPT 需要一个能直接访问存储的环境，例如设备上已有的 Linux 或取得 root 的 Android；维护者是用 `sgdisk` 修改的。改之前一定先备份分区表。

## 根文件系统

Debian 13（trixie）arm64，可以在电脑上用 `debootstrap` 配合 `qemu-user-static` 制作。主要软件：

| 类别 | 软件 |
|---|---|
| 基础 | NetworkManager、OpenSSH、btrfs-progs、nftables、avahi |
| 图形 | Mesa（Freedreno / Turnip）、greetd、Niri（自行编译，见 [从源码构建](building.md#niri)） |
| 音频 | PipeWire、WirePlumber（来自 `trixie-backports`），以及本仓库 [`audio/`](../audio/) 中的 ALSA UCM 配置 |
| 蓝牙 | BlueZ 5.88（Debian 13 自带的版本太旧，用 [`tools/bluez-backport.sh`](../tools/bluez-backport.sh) 编译） |
| 传感器 | `iio-sensor-proxy`（需要带 libssc 支持的版本）、`hexagonrpcd`、`libssc` |
| 输入法 | fcitx5 拼音 |

再装入与内核配套的模块，并把 [`system/`](../system/) 里的配置放到对应位置。[`tools/deploy-daily/`](../tools/deploy-daily/) 是维护者用的部署脚本，可以参考它的步骤，但它依赖维护者自己的环境，不能直接用于新设备。

## 固件

设备专有的固件不在仓库里，请从你自己设备的 Android（`vendor`、`modem`、`bluetooth` 等分区）中提取。需要的文件：

| 位置（`/usr/lib/firmware/` 下） | 内容 |
|---|---|
| `qcom/sm8650/lenovo/tb520fu/` | ADSP、CDSP（`adsp*`、`cdsp*` 及对应的 `.jsn`）、IPA、GPU zap（`gen70900_zap.mbn`）、触摸屏（`novatek_ts_*`）、视频（`vpu33_*`）、音频拓扑 |
| `qcom/vpu/vpu33_p4.mbn` | 视频硬件解码 |
| `ath12k/WCN7850/hw2.0/` | Wi‑Fi（`amss.bin`、`bdwlan*`） |
| `qca/` | 蓝牙（`hmtbtfw*`、`hmtnv*`） |
| `qcom/gen70900_*.fw`、`qcom/gmu_gen70900.bin` | GPU 微码，来自 [linux-firmware](https://git.kernel.org/pub/scm/linux/kernel/git/firmware/linux-firmware.git) |

## 刷入开机镜像

编好内核、装好模块后，打包 `boot_a` 镜像并用 fastboot 刷入，见 [从源码构建](building.md#开机镜像boot_a)。开机后在菜单里选 `Linux`。

刷入前请先读 [出问题怎么办](recovery.md)。

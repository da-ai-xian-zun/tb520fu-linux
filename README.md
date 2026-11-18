# tb520fu-linux

让联想 Yoga Pad Pro 12.7（TB520FU，骁龙 8 Gen 3）运行主线 Linux，并与原厂 Android 双系统共存。

*Mainline Linux for the Lenovo Yoga Pad Pro 12.7 (TB520FU, Snapdragon 8 Gen 3), dual-booting with stock Android. Documentation is in Chinese.*

开机时由 U-Boot 菜单选择进入 Linux 或 Android。Linux 一侧是 Debian 13 和 Niri 桌面，外壳、屏幕键盘和系统设置用 Rust 编写。

> 项目处于早期阶段。仓库提供内核补丁、设备树、引导程序、桌面组件和系统配置，暂时还没有安装程序；系统的组成和准备工作见 [安装概览](docs/install.md)。

## 主要功能

- **为平板打造的 Niri 桌面**（[`desktop-rust/`](desktop-rust/)）：用 Rust 编写的一整套桌面组件，搭配 Niri 使用。
  - 顶栏和控制中心：Wi‑Fi、蓝牙、亮度、音量、电源；
  - 锁屏、通知、应用菜单、手写笔工具；
  - 带拼音输入的屏幕键盘，拔下键盘保护套时自动弹出；
  - 系统设置：网络、蓝牙、显示（含 HDR）、声音、手写笔、电源等；
  - 后台服务：自动亮度、手写笔吸附充电、电源键。
- **在 Waydroid 中运行原厂 ZUI 18.5**（[说明](docs/waydroid-zui.md)，开发中）：用设备自带的 ZUI 系统镜像作为 Waydroid 容器，在 Linux 中运行安卓应用。目前已经能启动并用触控操作，其余功能还在完善。所需的内核支持已包含在本仓库的内核补丁中，容器脚本稳定后再发布。

## 支持情况

✅ 可用 · 🟡 部分可用 · ❌ 不支持

| 功能 | 状态 | 说明 |
|---|---|---|
| 屏幕 | ✅ | 2944×1840，120Hz |
| HDR | 🟡 | 内屏 HDR 可用；暂不支持 144Hz |
| 外接显示器 | ✅ | USB‑C 转 DisplayPort，最高 4K 144Hz；暂不支持 10 位色 |
| 触摸屏 | ✅ | |
| 手写笔 | 🟡 | 压感、吸附充电、电量显示可用；暂不支持振动 |
| 键盘保护套 | ✅ | 键盘、触控板，翻折自动切换平板模式 |
| 音频 | ✅ | 扬声器、麦克风 |
| Wi‑Fi / 蓝牙 | ✅ | |
| 休眠 | ✅ | 挂起到内存（S3） |
| USB‑C | ✅ | USB 3，自动切换主机/设备模式 |
| 传感器 | ✅ | 自动旋转、自动亮度 |
| 相机 | 🟡 | 三颗摄像头均可使用，后置主摄支持自动对焦；画质仍在调校 |
| 视频硬件解码 | ✅ | H.264、HEVC、VP9、AV1 |
| 充电 | ✅ | USB PD 快充 |
| 指纹 | ❌ | 开发中 |

## 开始之前

- **不需要解锁 bootloader，但要看 ABL 版本。** 本项目会替换 `boot_a` 分区，锁定状态下可以用 fastboot 刷写。镜像用公开的 AOSP 测试密钥签名。从 ZUI 17.5.10.362 起（包括 ZUI 18.5），原厂 ABL 不再信任这把密钥，锁定状态下无法启动本项目的镜像。维护者的设备使用的是 17.5.10.096 版本的 ABL。
- **注意回滚索引。** 镜像的回滚索引不能低于设备记录的值，否则无法启动；这个值只增不减。原厂 17.5.10.362 或更新的版本只要正常启动过，就会把它调高（18.5.10.037 为 `1785888000`），这时需要相应修改 [`uboot/build_uboot.sh`](uboot/build_uboot.sh) 中的 `RI`。
- **先完整备份。** 修改分区前，请备份 GPT 分区表以及 `persist`、`fsg`、`boot_a`、`init_boot_a`、`vbmeta_a` 等每台设备独有的分区。联想官方救砖工具的完整刷机会恢复原厂分区表，并清除为 Linux 新建的分区。
- **设备专属的值不在仓库里。** 蓝牙地址和 Wi‑Fi MAC 每台设备都不同。请将 [`device.conf.example`](device.conf.example) 复制为 `device.local.conf`，填入从自己设备的 Android 中读取的值；编译内核和部署系统配置时会用到它。
- **测试范围有限。** 目前只在一台设备上测试过（Android 17 / ZUI 18.5.10.037，天马屏幕）。其他固件版本和京东方屏幕的版本尚未验证。
- **本项目的全部代码由 AI 编写。** 刷机有变砖风险，请自行承担。

## 仓库结构

| 目录 | 内容 |
|---|---|
| [`kernel/`](kernel/) | 设备树、驱动、内核补丁、内核配置和编译脚本 |
| [`uboot/`](uboot/) | U-Boot 开机菜单，以及生成 `boot_a` 镜像的脚本 |
| [`boot-switch/`](boot-switch/) | 从 Linux 直接重启进入 Android |
| [`niri/`](niri/) | Niri 补丁：内屏 HDR、触控和触控板手势、护眼和阅读模式 |
| [`desktop-rust/`](desktop-rust/) | 桌面外壳、屏幕键盘、系统服务和系统设置 |
| [`camera/`](camera/) | libcamera 补丁、PipeWire 插件补丁和三颗摄像头的调校文件 |
| [`video/`](video/) | 视频硬件解码（VA-API）的说明和测试脚本 |
| [`audio/`](audio/) | ALSA UCM 配置 |
| [`system/`](system/) | 系统配置文件 |
| [`tools/`](tools/) | 部署脚本和蓝牙配对工具 |
| [`docs/`](docs/) | 文档：开机原理、安装概览、构建、硬件说明、桌面、手势、出问题怎么办 |

## 上游版本

| 组件 | 版本 |
|---|---|
| Linux | `v7.3.3` |
| Niri | `losnoco/niri` `8d2ecca6`、`losnoco/smithay` `b2d26fe77`（`color-management-features` 分支） |
| libcamera | `08c7982`（0.8.0） |
| 发行版 | Debian 13 |

## 版本号

版本号为 `主.次.修订`，按升级时是否需要刷机来区分：

- **主版本**：需要重新分区或重装系统。
- **次版本**：需要重新刷写 `boot_a`，例如更新内核、设备树或 U-Boot。
- **修订版本**：只更新软件和配置，不需要刷机。

每个 Release 会列出各组件的版本，并注明是否需要刷写 `boot_a`。

## 致谢

本项目建立在许多人的工作之上：

- Linux 内核及高通 SM8650 平台的维护者
- **GUF296**：拯救者 Y700 三代的 Linux 移植，包括设备树、屏幕、背光、触控和音频
- **joonhoekim**：内嵌设备树、开机画面和屏幕 DSC 的相关工作
- **Pengyu Luo**：`panel-novatek-nt36536` 驱动
- **Jianhua Lu**：`panel-novatek-nt36523` 驱动
- **map220v**：PS5169 USB‑C 重驱动芯片驱动
- **radxa-pkg/qcom-va-driver**：高通 Iris VA-API 驱动及内核补丁
- **Dawid Wróbel**：QSEECOM TEE 驱动、Goodix 指纹驱动和 `qsee-supplicant`
- **Osilvfe、mzak**：libcamera 手动对焦和对焦评分
- **hexagonrpcd、libssc** 以及 iio-sensor-proxy 的 libssc 支持：通过 ADSP 读取传感器
- **YaLTeR**：Niri
- **dividebysandwich、losnoco**：Niri 和 Smithay 的 HDR 支持

内核补丁中还包含 Awinic、Novatek、Goodix 等厂商驱动及其他开发者的代码，作者以各文件头的署名为准。

## 许可证

除另有标注外，本仓库代码和文档采用 [MIT](LICENSE) 许可证。各文件的许可证以文件头的 SPDX 标识为准：

| 部分 | 许可证 |
|---|---|
| 内核补丁和驱动 | GPL-2.0（个别文件为 GPL-2.0-only OR BSD-2-Clause） |
| 设备树 | BSD-3-Clause |
| U-Boot 补丁 | GPL-2.0+ |
| Niri 补丁、系统设置应用 `tb520fu-settings` | GPL-3.0-only |
| libcamera 补丁 | LGPL-2.1-or-later |
| 相机调校文件 | CC0-1.0 |

许可证全文见 [`LICENSES/`](LICENSES/)。

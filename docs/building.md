# 从源码构建

仓库里的构建脚本是维护者自己用的，路径和变量写在各脚本开头，使用前请先改成自己的环境。本文说明每个部分怎么构建、要注意什么。

## 准备

在 x86_64 的 Linux 电脑上交叉编译内核和 U-Boot（推荐 Debian 13）：

```bash
sudo apt install gcc-aarch64-linux-gnu make bc flex bison libssl-dev python3 device-tree-compiler git fastboot
```

打包开机镜像还需要 AOSP 的 `mkbootimg`、`avbtool`，以及 AOSP 测试密钥 `testkey_rsa4096.pem`（在 AOSP 源码的 `external/avb/test/data/` 下）。

Niri 和相机组件在平板上的 Debian 里直接编译。

## 内核

内核基于上游 Linux `v7.3.3`：

| 目录 | 内容 |
|---|---|
| [`kernel/patches/`](../kernel/patches/) | 补丁系列：`0001` 是设备移植，之后是 TEE、SELinux、视频硬件解码、Waydroid 容器所需的修改 |
| [`kernel/configs/`](../kernel/configs/) | 内核配置，当前使用 `config-7.3.3-full4` |
| [`kernel/initramfs/`](../kernel/initramfs/) | 内嵌的 initramfs |
| [`kernel/scripts/`](../kernel/scripts/) | 写入蓝牙地址的 `set-bdaddr.sh` |
| [`kernel/dts/`](../kernel/dts/)、[`kernel/drivers/`](../kernel/drivers/) | 设备树和几个驱动的单独副本，便于阅读（补丁里已包含） |

取得源码：

```bash
tar -xf linux-7.3.3.tar.xz && cd linux-7.3.3
git init -q && git add -A && git commit -q -m "Linux 7.3.3"
git am --keep-cr <本仓库>/kernel/patches/*.patch
cp <本仓库>/kernel/configs/config-7.3.3-full4 .config
```

部分厂商驱动的源文件使用 CRLF 行尾，`--keep-cr` 让它们保持原样。

配置里的 `CONFIG_INITRAMFS_SOURCE` 指向源码根目录下的 `initramfs.cpio`，请先用 [`kernel/initramfs/`](../kernel/initramfs/) 生成它。

### 编译步骤

```bash
export ARCH=arm64 CROSS_COMPILE=aarch64-linux-gnu-
make LOCALVERSION= olddefconfig
make LOCALVERSION= qcom/sm8650-lenovo-tb520fu.dtb
<本仓库>/kernel/scripts/set-bdaddr.sh arch/arm64/boot/dts/qcom/sm8650-lenovo-tb520fu.dtb
make LOCALVERSION= Image.gz modules
make LOCALVERSION= INSTALL_MOD_PATH=<输出目录> INSTALL_MOD_STRIP=1 modules_install
```

要点：

- **每次都显式传 `ARCH` 和 `CROSS_COMPILE`**。漏掉时 `make` 会按电脑本机的编译器调整配置，编出的内核和模块对不上。
- **设备树要单独先编。** 设备树内嵌在内核里，但 `Image` 目标不会自动重编它。
- **蓝牙地址在编译时写入。** 设备树源码里的蓝牙地址是全零占位，`set-bdaddr.sh` 会把 `device.local.conf` 里的 `BT_ADDR` 写进编好的设备树（见仓库根目录的 [`device.conf.example`](../device.conf.example)）。没有配置地址时脚本会报错退出。
- **改了 initramfs 要先删掉 `usr/initramfs_data.cpio*`**，否则不会重新嵌入。initramfs 需要静态编译的 arm64 `busybox` 和 `dmsetup`，见该目录的说明。
- **`LOCALVERSION=` 留空**，避免内核版本号被自动加上 `+`。

### 内核和模块必须一起更新

内核没有开启模块版本校验（`MODVERSIONS`）。只刷新内核、不换模块，新内核可能加载结构不兼容的旧模块，直接崩溃。建议：

- 每种配置用不同的 `CONFIG_LOCALVERSION`（例如 `-full4`），让模块装进各自的 `/usr/lib/modules/<版本>/`；
- 先把新模块装到平板上，再刷新的开机镜像；旧版本的模块目录保留，便于回退。

## 开机镜像（boot_a）

[`uboot/build_uboot.sh`](../uboot/build_uboot.sh) 编译 U-Boot，把三部分拼成一个镜像，再打包、签名：

1. **U-Boot**：上游 U-Boot `v2027.01-rc1`，打上 [`uboot/`](../uboot/) 里的补丁，并放入设备的配置、开机菜单脚本（`tb520fu.env`）和设备树。
2. **Android 内核**：从你设备当前的 `boot_a` 中提取（例如用 `mkbootimg` 附带的 `unpack_bootimg.py`），保存为 `out/android.Image`。它必须和你设备上 Android 的 vendor 模块匹配，所以要用自己设备上的。
3. **Linux 内核**：上一节编出的 `Image.gz`，用环境变量 `LINUX_GZ` 指定。

打包时：

- 用 `mkbootimg` 生成 header v4、不带 ramdisk 的镜像；
- 用 `avbtool add_hash_footer` 以 AOSP 测试密钥签名，分区大小 `100663296` 字节；
- 回滚索引 `RI` 和镜像属性里的系统版本信息写在脚本开头，请按自己的设备修改（见 [开机原理](boot.md#验证启动)）。

刷入：

```bash
fastboot flash boot_a out/boot_uboot_linux.img
fastboot reboot
```

刷之前务必保留一份原来的 `boot_a`，出问题时可以刷回去（见 [出问题怎么办](recovery.md)）。

## Niri

[`niri/build-niri-hdr.sh`](../niri/build-niri-hdr.sh) 在平板上以 root 运行：安装依赖，取 `losnoco/niri` 和 `losnoco/smithay` 的固定提交，打上 [`niri/patches/`](../niri/patches/) 的补丁后编译安装。需要 Rust 1.87 以上；Debian 13 自带的版本太旧，脚本会从 `trixie-backports` 安装。

补丁内容：

| 补丁 | 内容 |
|---|---|
| `0001` | 内屏 HDR：用背光余量在 SDR 屏幕上显示 HDR 高光 |
| `0002`–`0005` | 多指触控手势、护眼和阅读模式（硬件色彩矩阵）、触控拖动窗口、触控板手势 |

内屏 HDR 还需要在 Niri 配置中打开，见 [硬件说明](hardware.md#hdr)。

## 相机

[`camera/libcamera/build.sh`](../camera/libcamera/build.sh) 在平板上以桌面用户运行：取固定版本的上游 libcamera，打上补丁，编译安装到 `/opt/libcamera-tb520fu`；再按系统中的 PipeWire 版本重新编译它的 libcamera 插件，通过 systemd 用户服务的配置让 PipeWire 优先使用新插件，不覆盖系统文件。调校文件在 [`camera/tuning/`](../camera/tuning/)。

## 视频硬件解码

需要 [radxa-pkg/qcom-va-driver](https://github.com/radxa-pkg/qcom-va-driver) 的 VA-API 驱动，以及内核补丁系列中的 `0004`–`0008`。说明见 [`video/iris-vaapi/`](../video/iris-vaapi/)。

## 桌面

[`desktop-rust/`](../desktop-rust/) 是一个 Rust workspace。可以在平板上直接编译，也可以在电脑上交叉编译：

```bash
cargo build --release --target aarch64-unknown-linux-gnu
```

交叉编译使用 `scripts/aarch64-linker.sh` 链接；如果需要 arm64 的系统库，把 sysroot 放在仓库根目录的 `.arm64-sysroot`（或用环境变量 `TB520FU_ARM64_SYSROOT_LIB` 指定）。安装见 [桌面](desktop.md)。

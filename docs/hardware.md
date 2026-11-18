# 硬件说明

各硬件在 Linux 下的用法和限制。支持情况总表见 [首页](../README.md#支持情况)。

## 屏幕

- 2944×1840，120Hz，主线 `drm/msm` 驱动（双路 DSI + DSC）。Niri 中建议整数缩放 `scale 2.0`。
- GPU 为 Adreno 750，使用 Mesa 的 Freedreno（OpenGL）和 Turnip（Vulkan）。
- 暂不支持 144Hz：触摸屏的手写笔功能只针对 120Hz 标定，在 90Hz、144Hz 下没有笔坐标。
- 背光高亮模式：`/sys/class/backlight/ktz8866-backlight/hbm` 写入 `0`/`1`/`2`。超时、熄屏或休眠后会自动回到 `0`。
- **不要读取** `/sys/kernel/debug/dri/1/crtc-0/state`，它会访问受保护的寄存器。

## HDR

内屏本身只支持 SDR。Niri 的 HDR 补丁用背光余量显示 HDR 高光：画面中有 HDR 内容时调高背光，同时相应压低其余部分的亮度，让普通内容看起来不变。在 Niri 配置中打开：

```kdl
output "DSI-1" {
    hdr {
        panel-peak-luminance 900
    }
}
```

`900` 需要 `tb520fu-daemon` 配合开启背光高亮模式；不运行它时请用 `650`。系统设置的“显示”页面也有 HDR 开关。

播放 HDR 视频推荐 mpv：

```ini
vo=gpu-next
gpu-context=waylandvk
target-colorspace-hint=yes
hwdec=v4l2m2m-copy
```

## 外接显示器

- 通过 USB‑C 转 DisplayPort 输出，支持 DSC，最高 4K 144Hz。
- 暂不支持 10 位色输出，也没有 DisplayPort 音频。
- 触摸和手写笔始终作用于内屏。

## 音频

- 六个扬声器和两个内置麦克风通过 ALSA UCM 配置（[`audio/`](../audio/)）工作。
- [`system/`](../system/) 里有一套基础的扬声器保护：软件音量不超过 0 dB，扬声器前加 200 Hz 高通滤波。
- 支持蓝牙 LE Audio 耳机和 USB 声卡。

## 触摸屏、手写笔、键盘

- 触摸屏支持多指操作；配合 Niri 补丁有捏合进入总览、多指滑动切换列和工作区等手势，见 [触控与触控板手势](gestures.md)。
- 手写笔支持悬停、压感；吸附在机身上时自动无线充电（低于 85% 开始充，到 90% 停止），由 `tb520fu-daemon` 控制。暂不支持振动，侧键没有映射。
- 键盘保护套：按键、大写灯、触控板可用；翻折到背面时切换为平板模式，停用触控板。分屏、星标、应用键会上报为 `KEY_PROG1`–`KEY_PROG3`，默认没有绑定功能。

### 手写笔的蓝牙配对

Android 和 Linux 使用同一个蓝牙地址，所以手写笔的配对可以共用。但在任一侧重新配对后，另一侧保存的密钥就失效了，笔会反复断开重连。解决方法是把 Android 的配对密钥同步到 Linux：

1. 从 Android 中取出 `/data/misc/bluedroid/bt_config.conf`（需要 root）；
2. 在 Linux 中运行 `sudo python3 tools/bt-sync-android-keys.py bt_config.conf`。

## 休眠与电源

- 支持挂起到内存（S3）。电源键、键盘保护套、定时器可以唤醒，唤醒后屏幕自动点亮。
- 合上键盘保护套时休眠；默认空闲 5 分钟锁屏，不会自动休眠。
- CPU 最高 3.3 GHz（X4 大核），温控沿用原厂规则。
- 充电协议由设备固件处理，USB PD 快充与 Android 下相同。系统设置的“电池与电源”里可以打开旁路供电：充到 90% 后停止充电，改由电源直接供电，降到 85% 再恢复充电。

## Wi‑Fi 与蓝牙

- Wi‑Fi 和蓝牙芯片为 WCN7850。
- 两者的出厂地址都需要按设备配置：蓝牙地址在编译内核时写入设备树，Wi‑Fi MAC 通过 systemd 的 `.link` 文件设置（见 [`device.conf.example`](../device.conf.example)）。不配置时，Wi‑Fi 每次开机使用随机地址。
- 请使用 BlueZ 5.88 或更新版本，旧版本的 LE Audio 和手写笔连接有问题。

## USB‑C

- 自动切换主机和设备模式：接 U 盘、键盘接收器、USB 声卡等外设时作为主机（USB 3），接电脑时作为设备。
- 作为设备时只有 USB 2 速度，并提供一个串口控制台。

## 传感器

加速度计（自动旋转）和环境光（自动亮度）可用。它们由 ADSP 上的传感器中枢提供，需要 `hexagonrpcd`、`libssc` 和带 libssc 支持的 `iio-sensor-proxy`。陀螺仪、接近传感器等暂未接入。

## 相机

- 三颗摄像头：后置主摄（GC13A0，支持自动对焦）、后置微距（SC202CS）、前置（GC13A2）。
- 基于上游 libcamera，使用软件 ISP（GPU 去马赛克）。通过 PipeWire 提供给应用。
- 画质仍在调校：还没有镜头阴影校正，画面边缘会偏色、四角偏暗。

## 视频硬件解码

H.264、HEVC、VP9、AV1 可以硬件解码，通过 VA-API 提供给应用。Firefox 默认即使用硬件解码；mpv 的配置见上文 [HDR](#hdr)。

## 没有的硬件

这台设备没有 NFC 和机身振动马达。

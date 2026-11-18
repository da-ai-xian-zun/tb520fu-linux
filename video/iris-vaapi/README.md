# 视频硬件解码（VA-API）

让浏览器、mpv、GStreamer 通过标准的 VA-API 使用 `qcom-iris` 硬件解码器，支持 H.264、HEVC（含 10 位）、VP9、AV1（含 10 位）。

需要两部分：

1. **VA-API 驱动**：[radxa-pkg/qcom-va-driver](https://github.com/radxa-pkg/qcom-va-driver)，在平板上编译安装：

   ```bash
   meson setup build -Dbuildtype=release -Dcdsp_vpp=disabled --prefix=/usr --libdir=lib/aarch64-linux-gnu
   ninja -C build && sudo meson install -C build --no-rebuild
   ```

   依赖 `libva-dev libdrm-dev libgbm-dev libegl-dev libgles-dev meson`。libva 会按显卡驱动名自动加载它，不需要设置环境变量。

2. **内核补丁**：[`kernel/patches/`](../../kernel/patches/) 中的 `0004`–`0008`（来自同一个项目），其中 `0008` 是 AV1 正确解码所必需的。

Firefox 默认设置即会使用硬件解码。不要和其他 Iris VA-API 驱动的内核补丁混用，它们处理 AV1 隐藏帧的方式互不兼容。

## 测试脚本

- `vtest.sh <驱动目录> <驱动名> <片源目录> [片源…]`：逐帧比对 VA-API 硬件解码和软件解码的结果；
- `ffsample.sh [秒]`：采样正在运行的 Firefox 各进程的 CPU 占用和解码器中断数。

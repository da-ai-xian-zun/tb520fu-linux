# 桌面

桌面由 Niri 合成器和 [`desktop-rust/`](../desktop-rust/) 中的一组 Rust 程序组成，界面按平板触控设计，用 Vulkan 渲染。

## 组件

| 程序 | 作用 |
|---|---|
| `tb520fu-shell` | 桌面外壳：顶栏、下拉的控制中心、锁屏和待机显示（同时作为登录界面）、通知、应用菜单、手写笔批注和便签、壁纸 |
| `tb520fu-osk` | 屏幕键盘：停靠、分裂、悬浮三种形态；内置拼音，也可以配合 fcitx5 使用；拔下键盘保护套或翻折到背面时自动弹出 |
| `tb520fu-settings` | 系统设置：网络与飞行模式、蓝牙、显示（含 HDR、主题色、壁纸）、声音、通知与专注、电池与电源、手写笔和键盘、默认应用、密码、重启进 Android |
| `tb520fu-daemon` | 系统服务（root）：电源键和键盘保护套开合、手写笔吸附充电、自动亮度、背光高亮模式和 HDR 背光、旁路供电、静音指示灯、手电筒、重启进 Android |
| `fcitx5-addon/` | fcitx5 插件，让屏幕键盘显示输入法的候选词 |

控制中心里有 Wi‑Fi 和蓝牙的快速连接、音量和亮度调节、常用开关，以及“重启 / 进安卓 / 关机”按钮。

## Niri

需要打过本项目补丁的 Niri（见 [从源码构建](building.md#niri)）。补丁加入了多指捏合进入总览、多指滑动切换列和工作区、拖动窗口把手和分栏缝隙等触控操作，以及触控板手势，详见 [触控与触控板手势](gestures.md)。

另有护眼模式和阅读模式（用显示硬件的色彩矩阵实现），以及内屏 HDR（见 [硬件说明](hardware.md#hdr)）。

Niri 配置在 [`desktop-rust/niri/`](../desktop-rust/niri/)，登录界面的配置在 [`desktop-rust/greetd/`](../desktop-rust/greetd/)。

## 安装

1. 编译四个程序（见 [从源码构建](building.md#桌面)），把 `tb520fu-daemon`、`tb520fu-osk`、`tb520fu-shell`、`tb520fu-settings` 放到 `desktop-rust/dist/aarch64/`。
2. 在平板上运行：

   ```bash
   sudo TB520FU_USER=<你的用户名> ./desktop-rust/install-on-tablet.sh --with-deps --with-fcitx-addon
   ```

   - `--with-deps`：同时安装依赖的软件包；
   - `--with-fcitx-addon`：编译安装 fcitx5 插件。

脚本会安装程序、systemd 服务、greetd 登录配置、Niri 配置和 polkit 规则。壁纸放在 `desktop-rust/assets/wallpaper.jpg` 时会一并安装；仓库里不附带壁纸。

撤销安装：

```bash
sudo ./desktop-rust/install-on-tablet.sh --rollback
```

## 服务

| 服务 | 类型 |
|---|---|
| `tb520fu-daemon.service` | 系统服务 |
| `tb520fu-shell.service`、`tb520fu-osk.service`、`tb520fu-idle.service` | 用户服务，随 Niri 会话启动 |

`tb520fu-idle` 负责空闲锁屏，设置保存在 `~/.config/tb520fu/idle.conf`。

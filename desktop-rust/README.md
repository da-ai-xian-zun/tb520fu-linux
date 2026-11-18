# desktop-rust

联想 Yoga Pad Pro 12.7（TB520FU）的桌面组件，用 Rust 编写，搭配 Niri 使用，Vulkan 渲染。

| 目录 | 内容 |
|---|---|
| `crates/tb520fu-ui` | 公共底座：Vulkan 渲染、Wayland 协议、文字排版、图标、拼音、剪贴板 |
| `crates/tb520fu-shell` | 桌面外壳：顶栏、控制中心、锁屏和待机显示、通知、应用菜单、手写笔批注 |
| `crates/tb520fu-osk` | 屏幕键盘 |
| `crates/tb520fu-daemon` | 系统服务：电源键、键盘保护套、手写笔充电、自动亮度、背光高亮和 HDR、旁路供电等 |
| `crates/tb520fu-sys` | 系统控制库：NetworkManager、BlueZ、PipeWire、PAM、默认应用、Niri 配置等，无图形依赖 |
| `crates/tb520fu-settings` | 系统设置应用（Slint） |
| `fcitx5-addon/` | fcitx5 插件，让屏幕键盘显示候选词 |
| `niri/`、`greetd/`、`systemd/`、`polkit/` | Niri 配置、登录界面、服务和权限规则 |

构建和安装见 [从源码构建](../docs/building.md#桌面) 和 [桌面](../docs/desktop.md)。

许可证：`tb520fu-settings` 链接 Slint 社区版，为 GPL-3.0-only；其余为 MIT。

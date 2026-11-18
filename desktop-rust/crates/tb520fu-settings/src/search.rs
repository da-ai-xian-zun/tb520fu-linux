// SPDX-License-Identifier: GPL-3.0-only

#[derive(Debug, Clone)]
pub struct SearchEntry {
    pub page_id: &'static str,
    pub page_label: &'static str,
    pub section_tag: &'static str,
    pub title: &'static str,
    pub keywords: &'static str,
}

pub const SEARCH_ENTRIES: &[SearchEntry] = &[
    SearchEntry {
        page_id: "wifi",
        page_label: "无线局域网",
        section_tag: "wifi-sw",
        title: "无线局域网与网络共享",
        keywords: "wifi 无线 网络 ip 网关 mac 忘记 扫描 wlan 有线 usb 共享网络 ethernet",
    },
    SearchEntry {
        page_id: "wifi",
        page_label: "无线局域网",
        section_tag: "airplane",
        title: "飞行模式",
        keywords: "飞行 离线 射频 airplane flight mode",
    },
    SearchEntry {
        page_id: "bt",
        page_label: "蓝牙",
        section_tag: "bt-sw",
        title: "蓝牙开关与外设配对",
        keywords: "bluetooth 蓝牙 耳机 buds 配对 外设",
    },
    SearchEntry {
        page_id: "display",
        page_label: "显示与外观",
        section_tag: "skin",
        title: "深浅色界面外观",
        keywords: "外观 主题 深色 浅色 暗黑 暖纸 skin dark light",
    },
    SearchEntry {
        page_id: "display",
        page_label: "显示与外观",
        section_tag: "accent",
        title: "主题强调色",
        keywords: "强调色 主题色 颜色 蓝 紫 青 橙 红 绿 accent color",
    },
    SearchEntry {
        page_id: "display",
        page_label: "显示与外观",
        section_tag: "wallpaper",
        title: "桌面与锁屏壁纸",
        keywords: "壁纸 背景 锁屏壁纸 流羽 极光 星云 素雪 图库 wallpaper background",
    },
    SearchEntry {
        page_id: "display",
        page_label: "显示与外观",
        section_tag: "bright",
        title: "屏幕亮度与自动调节亮度",
        keywords: "亮度 自动亮度 光感 背光 lux brightness",
    },
    SearchEntry {
        page_id: "display",
        page_label: "显示与外观",
        section_tag: "hbm",
        title: "户外高亮模式",
        keywords: "hbm 高亮 强光 户外 峰值亮度",
    },
    SearchEntry {
        page_id: "display",
        page_label: "显示与外观",
        section_tag: "hdr",
        title: "HDR 高动态范围",
        keywords: "hdr edr 高动态范围 视频 峰值亮度 dolby pq",
    },
    SearchEntry {
        page_id: "display",
        page_label: "显示与外观",
        section_tag: "eyecare",
        title: "护眼模式",
        keywords: "护眼 蓝光 色温 夜览 night light ctm",
    },
    SearchEntry {
        page_id: "display",
        page_label: "显示与外观",
        section_tag: "reading",
        title: "阅读模式",
        keywords: "阅读 灰阶 黑白 墨水屏 reading",
    },
    SearchEntry {
        page_id: "display",
        page_label: "显示与外观",
        section_tag: "hires",
        title: "高刷新率 (120Hz / 60Hz)",
        keywords: "刷新率 高刷 120 144 60 hz",
    },
    SearchEntry {
        page_id: "display",
        page_label: "显示与外观",
        section_tag: "autorot",
        title: "自动旋转屏幕",
        keywords: "旋转 横屏 竖屏 重力感应 rotate",
    },
    SearchEntry {
        page_id: "display",
        page_label: "显示与外观",
        section_tag: "presets",
        title: "分栏宽度预设",
        keywords: "分栏 宽度 胶囊 预设 33 50 67 100 niri",
    },
    SearchEntry {
        page_id: "display",
        page_label: "显示与外观",
        section_tag: "scale",
        title: "显示缩放比例",
        keywords: "缩放 分辨率 dpi scale 2x",
    },
    SearchEntry {
        page_id: "display",
        page_label: "显示与外观",
        section_tag: "ext",
        title: "外接显示器",
        keywords: "外接 显示器 副屏 dp 4k monitor",
    },
    SearchEntry {
        page_id: "sound",
        page_label: "声音",
        section_tag: "vol",
        title: "主音量与扬声器输出",
        keywords: "声音 音量 静音 扬声器 输出 speaker volume mute",
    },
    SearchEntry {
        page_id: "sound",
        page_label: "声音",
        section_tag: "mic",
        title: "麦克风增益与输入设备",
        keywords: "麦克风 录音 输入 增益 mic pipewire",
    },
    SearchEntry {
        page_id: "notif",
        page_label: "通知与专注",
        section_tag: "dnd",
        title: "勿扰模式与全屏自动勿扰",
        keywords: "勿扰 专注 免打扰 全屏 dnd focus notifications",
    },
    SearchEntry {
        page_id: "notif",
        page_label: "通知与专注",
        section_tag: "dnd-sched",
        title: "定时勿扰",
        keywords: "定时 勿扰 睡眠 日程 时间段 schedule",
    },
    SearchEntry {
        page_id: "notif",
        page_label: "通知与专注",
        section_tag: "banner",
        title: "横幅停留时长",
        keywords: "通知 横幅 弹窗 时长 banner timeout",
    },
    SearchEntry {
        page_id: "notif",
        page_label: "通知与专注",
        section_tag: "lock-notif",
        title: "锁屏通知显示",
        keywords: "锁屏通知 隐私 隐藏 正文 脱敏 lockscreen redact",
    },
    SearchEntry {
        page_id: "notif",
        page_label: "通知与专注",
        section_tag: "notif-apps",
        title: "应用通知开关",
        keywords: "应用通知 屏蔽 应用权限 app notification mute",
    },
    SearchEntry {
        page_id: "power",
        page_label: "电池与电源",
        section_tag: "bypass",
        title: "90% 旁路供电",
        keywords: "旁路 护电 充电 90 电池 功率 bypass charge",
    },
    SearchEntry {
        page_id: "power",
        page_label: "电池与电源",
        section_tag: "profile",
        title: "性能模式 (省电 / 均衡 / 性能)",
        keywords: "性能 省电 均衡 cpu 调度 power profile",
    },
    SearchEntry {
        page_id: "power",
        page_label: "电池与电源",
        section_tag: "idle",
        title: "空闲自动锁屏与自动休眠",
        keywords: "锁屏 休眠 待机 息屏 idle sleep",
    },
    SearchEntry {
        page_id: "power",
        page_label: "电池与电源",
        section_tag: "suslog",
        title: "休眠功耗记录",
        keywords: "掉电 续航 休眠记录 s3 日志",
    },
    SearchEntry {
        page_id: "pen",
        page_label: "手写笔与键鼠",
        section_tag: "pen-bat",
        title: "Lenovo Tab Pen Pro 电量与充电",
        keywords: "手写笔 笔 电量 无线充 85 90 pen stylus",
    },
    SearchEntry {
        page_id: "pen",
        page_label: "手写笔与键鼠",
        section_tag: "pen-gest",
        title: "底角上滑手势与手写笔悬浮球",
        keywords: "底角 角落 手势 悬浮球 笔球 corner orb",
    },
    SearchEntry {
        page_id: "pen",
        page_label: "手写笔与键鼠",
        section_tag: "touchpad",
        title: "触控板 (轻触点击 / 自然滚动 / 速度)",
        keywords: "触控板 轻触 自然滚动 速度 右键 touchpad",
    },
    SearchEntry {
        page_id: "pen",
        page_label: "手写笔与键鼠",
        section_tag: "osk",
        title: "屏幕键盘形态、自动弹出与按键音效",
        keywords: "屏幕键盘 软键盘 虚拟键盘 分裂 悬浮 按键音 osk virtual keyboard",
    },
    SearchEntry {
        page_id: "pen",
        page_label: "手写笔与键鼠",
        section_tag: "ime",
        title: "输入法切换快捷键与 Caps Lock 映射",
        keywords: "输入法 拼音 中英切换 fcitx5 capslock ctrl shift space",
    },
    SearchEntry {
        page_id: "pen",
        page_label: "手写笔与键鼠",
        section_tag: "shortcuts",
        title: "系统快捷键总览",
        keywords: "快捷键 热键 键位 批注 便签 总览 shortcut hotkey super",
    },
    SearchEntry {
        page_id: "sys",
        page_label: "系统与关于",
        section_tag: "default-apps",
        title: "默认应用 (浏览器 / PDF / 图片 / 音视频)",
        keywords: "默认应用 打开方式 浏览器 pdf 图片 播放器 mimeapps xdg default browser papers loupe",
    },
    SearchEntry {
        page_id: "sys",
        page_label: "系统与关于",
        section_tag: "passwd",
        title: "登录密码修改",
        keywords: "密码 锁屏密码 账户 安全 pam passwd password",
    },
    SearchEntry {
        page_id: "sys",
        page_label: "系统与关于",
        section_tag: "tz",
        title: "日期、时间、时区与系统语言",
        keywords: "时区 时间 语言 中文 ntp timezone locale",
    },
    SearchEntry {
        page_id: "sys",
        page_label: "系统与关于",
        section_tag: "boot",
        title: "重启进入 Android",
        keywords: "安卓 双系统 引导 重启 android zui",
    },
    SearchEntry {
        page_id: "sys",
        page_label: "系统与关于",
        section_tag: "about",
        title: "关于本机、设备名称与存储空间",
        keywords: "关于 本机 主机名 内核 存储 磁盘 清理 内存 sm8650 tb520fu baobab",
    },
];

pub fn query_search(raw: &str) -> Vec<&'static SearchEntry> {
    let q = raw.trim().to_lowercase();
    if q.is_empty() {
        return Vec::new();
    }
    let tokens: Vec<&str> = q.split_whitespace().collect();
    SEARCH_ENTRIES
        .iter()
        .filter(|e| {
            let hay = format!("{} {} {}", e.title, e.page_label, e.keywords).to_lowercase();
            tokens.iter().all(|t| hay.contains(t))
        })
        .collect()
}

pub fn normalize_page_id(raw: &str) -> &'static str {
    match raw.trim().to_ascii_lowercase().as_str() {
        "wifi" | "network" | "net" | "wlan" => "wifi",
        "bt" | "bluetooth" => "bt",
        "display" | "screen" | "appearance" | "wallpaper" => "display",
        "sound" | "audio" | "volume" => "sound",
        "notif" | "notifications" | "focus" | "dnd" => "notif",
        "power" | "battery" => "power",
        "pen" | "stylus" | "touchpad" | "keyboard" | "osk" => "pen",
        "sys" | "system" | "about" | "apps" => "sys",
        _ => "display",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_finds_chinese_and_english_keywords() {
        let h1 = query_search("旁路");
        assert_eq!(h1.len(), 1);
        assert_eq!(h1[0].page_id, "power");

        let h2 = query_search("壁纸");
        assert_eq!(h2.len(), 1);
        assert_eq!(h2[0].section_tag, "wallpaper");

        let h3 = query_search("默认应用");
        assert_eq!(h3.len(), 1);
        assert_eq!(h3[0].page_id, "sys");

        let h4 = query_search("勿扰");
        assert!(h4.iter().any(|e| e.page_id == "notif"));
        assert_eq!(normalize_page_id("notifications"), "notif");
    }
}

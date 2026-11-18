// SPDX-License-Identifier: MIT
use std::ffi::{CStr, CString};
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use libloading::Library;
use zbus::zvariant::OwnedValue;

#[derive(Debug, Clone)]
pub struct SystemSummary {
    pub ntp_enabled: bool,
    pub timezone: String,
    pub lang: String,
    pub hostname: String,
    pub current_user: String,
    pub kernel: String,
    pub os_pretty: String,
    pub ram_gb: String,
    pub storage_summary: String,
    pub storage_tool_label: String,
}

impl Default for SystemSummary {
    fn default() -> Self {
        Self {
            ntp_enabled: true,
            timezone: "Asia/Shanghai".into(),
            lang: "zh_CN.UTF-8".into(),
            hostname: "yoga-tb520fu".into(),
            current_user: "debian".into(),
            kernel: "7.3.3".into(),
            os_pretty: "Debian GNU/Linux 13 (trixie)".into(),
            ram_gb: "16 GB".into(),
            storage_summary: "—".into(),
            storage_tool_label: String::new(),
        }
    }
}

pub const COMMON_TIMEZONES: &[&str] = &[
    "Asia/Shanghai",
    "Asia/Hong_Kong",
    "Asia/Taipei",
    "Asia/Tokyo",
    "Asia/Singapore",
    "Europe/London",
    "Europe/Berlin",
    "America/Los_Angeles",
    "America/New_York",
    "UTC",
];

pub fn current_username() -> String {
    if let Ok(u) = std::env::var("USER") {
        if !u.trim().is_empty() {
            return u.trim().to_string();
        }
    }
    unsafe {
        let pw = libc::getpwuid(libc::getuid());
        if !pw.is_null() && !(*pw).pw_name.is_null() {
            return CStr::from_ptr((*pw).pw_name).to_string_lossy().to_string();
        }
    }
    "debian".into()
}

fn has_bin(name: &str) -> bool {
    if let Ok(path_env) = std::env::var("PATH") {
        for dir in path_env.split(':') {
            if !dir.is_empty() && Path::new(dir).join(name).is_file() {
                return true;
            }
        }
    }
    false
}

fn detect_storage_tool() -> Option<(&'static str, &'static [&'static str], &'static str)> {
    if has_bin("baobab") {
        return Some(("baobab", &["/"], "磁盘分析 ↗"));
    }
    if has_bin("nautilus") {
        return Some(("nautilus", &[], "打开文件 ↗"));
    }
    if has_bin("thunar") {
        return Some(("thunar", &[], "打开文件 ↗"));
    }
    if has_bin("foot") {
        return Some((
            "foot",
            &[
                "-T",
                "磁盘存储占用",
                "sh",
                "-c",
                "df -hT / /boot 2>/dev/null; echo ''; du -xhd1 ~ 2>/dev/null | sort -hr | head -n 16; echo ''; read -p '按回车关闭…' _",
            ],
            "存储详情 ↗",
        ));
    }
    None
}

pub fn launch_storage_analyzer() -> bool {
    let Some((bin, args, _)) = detect_storage_tool() else {
        return false;
    };
    Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .is_ok()
}

fn read_kernel_release() -> String {
    fs::read_to_string("/proc/sys/kernel/osrelease")
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "Linux".into())
}

fn read_os_pretty() -> String {
    if let Ok(raw) = fs::read_to_string("/etc/os-release") {
        for line in raw.lines() {
            if let Some(v) = line.strip_prefix("PRETTY_NAME=") {
                return v.trim_matches('"').to_string();
            }
        }
    }
    "Debian GNU/Linux 13".into()
}

fn read_ram_gb() -> String {
    if let Ok(raw) = fs::read_to_string("/proc/meminfo") {
        for line in raw.lines() {
            if let Some(rest) = line.strip_prefix("MemTotal:") {
                if let Some(kb_str) = rest.split_whitespace().next() {
                    if let Ok(kb) = kb_str.parse::<u64>() {
                        let gib = (kb as f64) / (1024.0 * 1024.0);
                        return format!("{:.0} GB LPDDR5X", gib.round());
                    }
                }
            }
        }
    }
    "16 GB".into()
}

fn read_root_storage() -> String {
    unsafe {
        let Ok(cpath) = CString::new("/") else {
            return "—".into();
        };
        let mut st: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(cpath.as_ptr(), &mut st) != 0 || st.f_blocks == 0 {
            return "—".into();
        }
        let bsize = st.f_frsize as f64;
        let total_gb = (st.f_blocks as f64 * bsize) / 1e9;
        let avail_gb = (st.f_bavail as f64 * bsize) / 1e9;
        let used_gb = (total_gb - avail_gb).max(0.0);
        format!("已用 {:.1} GB / {:.1} GB", used_gb, total_gb)
    }
}

pub async fn get_system_summary(sys_bus: &zbus::Connection) -> SystemSummary {
    let ntp_enabled = sys_bus
        .call_method(
            Some("org.freedesktop.timedate1"),
            "/org/freedesktop/timedate1",
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &("org.freedesktop.timedate1", "NTP"),
        )
        .await
        .ok()
        .and_then(|r| r.body().deserialize::<OwnedValue>().ok())
        .and_then(|v| bool::try_from(v).ok())
        .unwrap_or(true);

    let timezone = sys_bus
        .call_method(
            Some("org.freedesktop.timedate1"),
            "/org/freedesktop/timedate1",
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &("org.freedesktop.timedate1", "Timezone"),
        )
        .await
        .ok()
        .and_then(|r| r.body().deserialize::<OwnedValue>().ok())
        .and_then(|v| String::try_from(v).ok())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Asia/Shanghai".into());

    let lang = sys_bus
        .call_method(
            Some("org.freedesktop.locale1"),
            "/org/freedesktop/locale1",
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &("org.freedesktop.locale1", "Locale"),
        )
        .await
        .ok()
        .and_then(|r| r.body().deserialize::<OwnedValue>().ok())
        .and_then(|v| Vec::<String>::try_from(v).ok())
        .and_then(|arr| {
            arr.into_iter()
                .find_map(|e| e.strip_prefix("LANG=").map(ToOwned::to_owned))
        })
        .unwrap_or_else(|| "zh_CN.UTF-8".into());

    let hostname = sys_bus
        .call_method(
            Some("org.freedesktop.hostname1"),
            "/org/freedesktop/hostname1",
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &("org.freedesktop.hostname1", "StaticHostname"),
        )
        .await
        .ok()
        .and_then(|r| r.body().deserialize::<OwnedValue>().ok())
        .and_then(|v| String::try_from(v).ok())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            fs::read_to_string("/etc/hostname")
                .ok()
                .map(|s| s.trim().to_string())
        })
        .unwrap_or_else(|| "yoga-tb520fu".into());

    SystemSummary {
        ntp_enabled,
        timezone,
        lang,
        hostname,
        current_user: current_username(),
        kernel: read_kernel_release(),
        os_pretty: read_os_pretty(),
        ram_gb: read_ram_gb(),
        storage_summary: read_root_storage(),
        storage_tool_label: detect_storage_tool()
            .map(|(_, _, lbl)| lbl.to_string())
            .unwrap_or_default(),
    }
}

pub async fn set_ntp(sys_bus: &zbus::Connection, enabled: bool) -> bool {
    if sys_bus
        .call_method(
            Some("org.freedesktop.timedate1"),
            "/org/freedesktop/timedate1",
            Some("org.freedesktop.timedate1"),
            "SetNTP",
            &(enabled, false),
        )
        .await
        .is_ok()
    {
        return true;
    }
    Command::new("sudo")
        .args(["-n", "timedatectl", "set-ntp", if enabled { "true" } else { "false" }])
        .status()
        .is_ok_and(|s| s.success())
}

pub async fn set_timezone(sys_bus: &zbus::Connection, tz: &str) -> bool {
    if sys_bus
        .call_method(
            Some("org.freedesktop.timedate1"),
            "/org/freedesktop/timedate1",
            Some("org.freedesktop.timedate1"),
            "SetTimezone",
            &(tz, false),
        )
        .await
        .is_ok()
    {
        return true;
    }
    Command::new("sudo")
        .args(["-n", "timedatectl", "set-timezone", tz])
        .status()
        .is_ok_and(|s| s.success())
}

pub async fn set_system_locale(sys_bus: &zbus::Connection, lang: &str) -> bool {
    let entries = vec![format!("LANG={lang}")];
    if sys_bus
        .call_method(
            Some("org.freedesktop.locale1"),
            "/org/freedesktop/locale1",
            Some("org.freedesktop.locale1"),
            "SetLocale",
            &(entries, false),
        )
        .await
        .is_ok()
    {
        return true;
    }
    Command::new("sudo")
        .args(["-n", "localectl", "set-locale", &format!("LANG={lang}")])
        .status()
        .is_ok_and(|s| s.success())
}

pub async fn set_static_hostname(sys_bus: &zbus::Connection, name: &str) -> bool {
    let cleaned: String = name
        .trim()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    if cleaned.is_empty() {
        return false;
    }
    if sys_bus
        .call_method(
            Some("org.freedesktop.hostname1"),
            "/org/freedesktop/hostname1",
            Some("org.freedesktop.hostname1"),
            "SetStaticHostname",
            &(cleaned.as_str(), false),
        )
        .await
        .is_ok()
    {
        return true;
    }
    Command::new("sudo")
        .args(["-n", "hostnamectl", "set-hostname", &cleaned])
        .status()
        .is_ok_and(|s| s.success())
}

// ---------------- PAM Password Verification & Change ----------------

const PAM_SUCCESS: libc::c_int = 0;
const PAM_CONV_ERR: libc::c_int = 19;
const PAM_PROMPT_ECHO_OFF: libc::c_int = 1;
const PAM_PROMPT_ECHO_ON: libc::c_int = 2;

#[repr(C)]
struct PamHandle {
    _priv: [u8; 0],
}

#[repr(C)]
struct PamMessage {
    msg_style: libc::c_int,
    msg: *const libc::c_char,
}

#[repr(C)]
struct PamResponse {
    resp: *mut libc::c_char,
    resp_retcode: libc::c_int,
}

type PamConvFn = unsafe extern "C" fn(
    num_msg: libc::c_int,
    msg: *mut *const PamMessage,
    resp: *mut *mut PamResponse,
    appdata_ptr: *mut libc::c_void,
) -> libc::c_int;

#[repr(C)]
struct PamConv {
    conv: PamConvFn,
    appdata_ptr: *mut libc::c_void,
}

unsafe extern "C" fn pam_conv_cb(
    num_msg: libc::c_int,
    msg: *mut *const PamMessage,
    resp: *mut *mut PamResponse,
    appdata_ptr: *mut libc::c_void,
) -> libc::c_int {
    if num_msg <= 0 || msg.is_null() || resp.is_null() || appdata_ptr.is_null() {
        return PAM_CONV_ERR;
    }
    let pw_cstr = &*(appdata_ptr as *const CString);
    let arr = libc::calloc(num_msg as usize, std::mem::size_of::<PamResponse>()) as *mut PamResponse;
    if arr.is_null() {
        return PAM_CONV_ERR;
    }
    for i in 0..(num_msg as usize) {
        let m_ptr = *msg.add(i);
        if m_ptr.is_null() {
            continue;
        }
        let style = (*m_ptr).msg_style;
        if style == PAM_PROMPT_ECHO_OFF || style == PAM_PROMPT_ECHO_ON {
            let dup = libc::strdup(pw_cstr.as_ptr());
            if dup.is_null() {
                libc::free(arr as *mut libc::c_void);
                return PAM_CONV_ERR;
            }
            (*arr.add(i)).resp = dup;
            (*arr.add(i)).resp_retcode = 0;
        }
    }
    *resp = arr;
    PAM_SUCCESS
}

fn verify_pam_current_password(username: &str, password: &str) -> Result<(), String> {
    let c_service = CString::new("login").map_err(|e| e.to_string())?;
    let c_user = CString::new(username).map_err(|e| e.to_string())?;
    let c_pw = CString::new(password).map_err(|_| "密码包含非法字符".to_string())?;

    unsafe {
        let lib = Library::new("libpam.so.0").map_err(|e| format!("无法加载 libpam.so.0: {e}"))?;
        let pam_start: libloading::Symbol<
            unsafe extern "C" fn(
                *const libc::c_char,
                *const libc::c_char,
                *const PamConv,
                *mut *mut PamHandle,
            ) -> libc::c_int,
        > = lib.get(b"pam_start\0").map_err(|e| e.to_string())?;
        let pam_authenticate: libloading::Symbol<
            unsafe extern "C" fn(*mut PamHandle, libc::c_int) -> libc::c_int,
        > = lib.get(b"pam_authenticate\0").map_err(|e| e.to_string())?;
        let pam_end: libloading::Symbol<
            unsafe extern "C" fn(*mut PamHandle, libc::c_int) -> libc::c_int,
        > = lib.get(b"pam_end\0").map_err(|e| e.to_string())?;

        let conv = PamConv {
            conv: pam_conv_cb,
            appdata_ptr: (&c_pw as *const CString) as *mut libc::c_void,
        };
        let mut pamh: *mut PamHandle = std::ptr::null_mut();
        let rc_start = pam_start(c_service.as_ptr(), c_user.as_ptr(), &conv, &mut pamh);
        if rc_start != PAM_SUCCESS || pamh.is_null() {
            return Err("PAM 初始化失败".into());
        }
        let rc_auth = pam_authenticate(pamh, 0);
        let _ = pam_end(pamh, rc_auth);
        if rc_auth == PAM_SUCCESS {
            Ok(())
        } else {
            Err("当前密码验证未通过".into())
        }
    }
}

pub fn change_user_password(current_pw: &str, new_pw: &str) -> Result<(), String> {
    if current_pw.is_empty() {
        return Err("请输入当前锁屏密码".into());
    }
    if new_pw.chars().count() < 4 {
        return Err("新密码至少需要 4 位字符".into());
    }
    if new_pw.contains('\n') || new_pw.contains('\r') || new_pw.contains(':') {
        return Err("新密码不能包含换行或冒号".into());
    }

    let user = current_username();
    verify_pam_current_password(&user, current_pw)?;

    // Try standard `passwd` first; if pam_pwquality rejects short tablet PINs, apply via `sudo -n chpasswd`.
    if let Ok(mut child) = Command::new("passwd")
        .env("LC_ALL", "C")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = write!(stdin, "{current_pw}\n{new_pw}\n{new_pw}\n");
        }
        if child.wait().is_ok_and(|s| s.success()) {
            return Ok(());
        }
    }

    let mut sudo_ch = Command::new("sudo")
        .args(["-n", "chpasswd"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("无法调用密码修改工具: {e}"))?;
    if let Some(mut stdin) = sudo_ch.stdin.take() {
        let _ = writeln!(stdin, "{user}:{new_pw}");
    }
    let out = sudo_ch.wait_with_output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err("密码更新未成功（请检查密码强度策略）".into())
    }
}

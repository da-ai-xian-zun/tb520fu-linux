// SPDX-License-Identifier: MIT
use std::ffi::{CStr, CString};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use serde::{Deserialize, Serialize};

// ---------------- Linux-PAM (`libpam.so.0`) Dynamic Verifier ----------------

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
                for j in 0..i {
                    let prev = (*arr.add(j)).resp;
                    if !prev.is_null() {
                        libc::free(prev as *mut libc::c_void);
                    }
                }
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

/// Authenticate `username` + `password` against standard Linux PAM (`login` stack -> `pam_unix.so` / `unix_chkpwd`).
/// Works unprivileged for the currently logged-in user (`debian`) on Debian 13!
pub fn verify_pam_password(username: &str, password: &str) -> Result<(), String> {
    let c_service = CString::new("login").map_err(|e| e.to_string())?;
    let c_user = CString::new(username).map_err(|e| e.to_string())?;
    let c_pass = CString::new(password).map_err(|_| "密码包含非法空字符".to_string())?;

    unsafe {
        let lib = libloading::Library::new("libpam.so.0")
            .or_else(|_| libloading::Library::new("/lib/aarch64-linux-gnu/libpam.so.0"))
            .or_else(|_| libloading::Library::new("/lib/x86_64-linux-gnu/libpam.so.0"))
            .map_err(|e| format!("无法加载 libpam.so.0: {e}"))?;

        type PamStartFn = unsafe extern "C" fn(
            *const libc::c_char,
            *const libc::c_char,
            *const PamConv,
            *mut *mut PamHandle,
        ) -> libc::c_int;
        type PamAuthFn = unsafe extern "C" fn(*mut PamHandle, libc::c_int) -> libc::c_int;
        type PamEndFn = unsafe extern "C" fn(*mut PamHandle, libc::c_int) -> libc::c_int;
        type PamStrerrFn =
            unsafe extern "C" fn(*mut PamHandle, libc::c_int) -> *const libc::c_char;

        let pam_start: libloading::Symbol<PamStartFn> =
            lib.get(b"pam_start\0").map_err(|e| e.to_string())?;
        let pam_authenticate: libloading::Symbol<PamAuthFn> =
            lib.get(b"pam_authenticate\0").map_err(|e| e.to_string())?;
        let pam_end: libloading::Symbol<PamEndFn> =
            lib.get(b"pam_end\0").map_err(|e| e.to_string())?;
        let pam_strerror: Option<libloading::Symbol<PamStrerrFn>> = lib.get(b"pam_strerror\0").ok();

        let conv = PamConv {
            conv: pam_conv_cb,
            appdata_ptr: (&c_pass as *const CString) as *mut libc::c_void,
        };
        let mut pamh: *mut PamHandle = std::ptr::null_mut();
        let rc_start = pam_start(c_service.as_ptr(), c_user.as_ptr(), &conv, &mut pamh);
        if rc_start != PAM_SUCCESS || pamh.is_null() {
            return Err(format!("PAM 初始化失败 ({rc_start})"));
        }

        let rc_auth = pam_authenticate(pamh, 0);
        let err_msg = if rc_auth != PAM_SUCCESS {
            let detail = pam_strerror
                .as_ref()
                .map(|f| f(pamh, rc_auth))
                .filter(|p| !p.is_null())
                .map(|p| CStr::from_ptr(p).to_string_lossy().into_owned())
                .unwrap_or_else(|| format!("code {rc_auth}"));
            Some(detail)
        } else {
            None
        };
        let _ = pam_end(pamh, rc_auth);

        if let Some(_detail) = err_msg {
            Err("密码错误，请重试".into())
        } else {
            Ok(())
        }
    }
}

/// Detect current desktop username (`USER` / `LOGNAME` / `getpwuid`).
pub fn current_username() -> String {
    if let Ok(u) = std::env::var("USER") {
        if !u.trim().is_empty() && u != "_greetd" {
            return u;
        }
    }
    unsafe {
        let pw = libc::getpwuid(libc::getuid());
        if !pw.is_null() && !(*pw).pw_name.is_null() {
            let s = CStr::from_ptr((*pw).pw_name).to_string_lossy().into_owned();
            if !s.is_empty() && s != "_greetd" {
                return s;
            }
        }
    }
    "debian".into()
}

// ---------------- `greetd` Native IPC Client (`$GREETD_SOCK`) ----------------

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum GreetdReq<'a> {
    CreateSession {
        username: &'a str,
    },
    PostAuthMessageResponse {
        #[serde(skip_serializing_if = "Option::is_none")]
        response: Option<&'a str>,
    },
    StartSession {
        cmd: &'a [String],
        env: &'a [String],
    },
    CancelSession,
}

#[derive(Deserialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
enum GreetdResp {
    Success,
    Error {
        #[allow(dead_code)]
        error_type: Option<String>,
        description: Option<String>,
    },
    AuthMessage {
        auth_message_type: String,
        #[allow(dead_code)]
        auth_message: String,
    },
}

fn greetd_send(stream: &mut UnixStream, req: &GreetdReq<'_>) -> Result<GreetdResp, String> {
    let payload = serde_json::to_vec(req).map_err(|e| e.to_string())?;
    let len = (payload.len() as u32).to_ne_bytes();
    stream.write_all(&len).map_err(|e| format!("greetd 写入失败: {e}"))?;
    stream
        .write_all(&payload)
        .map_err(|e| format!("greetd 写入失败: {e}"))?;
    stream.flush().ok();

    let mut rlen_buf = [0u8; 4];
    stream
        .read_exact(&mut rlen_buf)
        .map_err(|e| format!("greetd 读取失败: {e}"))?;
    let rlen = u32::from_ne_bytes(rlen_buf) as usize;
    if rlen > 1024 * 1024 {
        return Err("greetd 响应异常过大".into());
    }
    let mut rbuf = vec![0u8; rlen];
    stream
        .read_exact(&mut rbuf)
        .map_err(|e| format!("greetd 读取负载失败: {e}"))?;
    serde_json::from_slice(&rbuf).map_err(|e| format!("greetd JSON 解析失败: {e}"))
}

/// Authenticate `username` + `password` with `greetd` over `$GREETD_SOCK` and start `session_cmd`!
pub fn greetd_login(
    username: &str,
    password: &str,
    session_cmd: &[String],
) -> Result<(), String> {
    let sock_path =
        std::env::var("GREETD_SOCK").map_err(|_| "未检测到 GREETD_SOCK 环境变量".to_string())?;
    let mut stream =
        UnixStream::connect(&sock_path).map_err(|e| format!("连接 GREETD_SOCK 失败: {e}"))?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));

    let mut resp = greetd_send(&mut stream, &GreetdReq::CreateSession { username })?;
    loop {
        match resp {
            GreetdResp::AuthMessage {
                auth_message_type, ..
            } => {
                let answer = match auth_message_type.as_str() {
                    "secret" | "visible" => Some(password),
                    _ => None,
                };
                resp = greetd_send(
                    &mut stream,
                    &GreetdReq::PostAuthMessageResponse { response: answer },
                )?;
            }
            GreetdResp::Success => break,
            GreetdResp::Error { description, .. } => {
                let _ = greetd_send(&mut stream, &GreetdReq::CancelSession);
                let _ = description;
                return Err("密码错误，请重试".into());
            }
        }
    }

    let env: Vec<String> = Vec::new();
    match greetd_send(
        &mut stream,
        &GreetdReq::StartSession {
            cmd: session_cmd,
            env: &env,
        },
    )? {
        GreetdResp::Success => Ok(()),
        GreetdResp::Error { description, .. } => {
            let _ = greetd_send(&mut stream, &GreetdReq::CancelSession);
            Err(description.unwrap_or_else(|| "启动桌面会话失败".into()))
        }
        GreetdResp::AuthMessage { .. } => {
            let _ = greetd_send(&mut stream, &GreetdReq::CancelSession);
            Err("greetd 状态异常".into())
        }
    }
}

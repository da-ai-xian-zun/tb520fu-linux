// SPDX-License-Identifier: MIT
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use zbus::Connection;

use crate::kbd_watch::KbdState;
use crate::layouts::Lang;

const MAX_CANDIDATES: usize = 240;

/// Events arriving from `fcitx5` or D-Bus callers -> Main Wayland UI loop.
#[derive(Debug, Clone)]
pub enum FcitxEvent {
    ShowRequested { forced: bool },
    HideRequested { forced: bool },
    ToggleRequested,
    PreeditChanged(String),
    CandidatesChanged {
        list: Vec<String>,
        has_prev: bool,
        has_next: bool,
        cursor: i32,
    },
    LangChanged(Lang),
    FallbackPasteCommit(String),
    /// D-Bus `fyi.tb520fu.Osk.prime` caller asks to (re-)prime fcitx5 on-screen mode.
    PrimeRequested,
    /// D-Bus `fyi.tb520fu.Osk.reload_prefs` caller asks OSK to reload `~/.config/tb520fu/osk.json`.
    ReloadPrefs,
}

/// Commands sent from Main Wayland UI loop -> Async `zbus` Fcitx5 actor.
#[derive(Debug, Clone)]
pub enum FcitxCmd {
    TypeKey(String),
    CommitString(String),
    SelectCandidate(i32),
    Page { next: bool },
    SetLang(Lang),
    EnterOnScreenMode,
    RequestHide,
    ClearComposition,
    /// Make fcitx5 enter `OnScreenKeyboard` mode without leaving the OSK visible.
    /// Triggered at startup, on keyboard detach / tablet-mode fold, and on fcitx5
    /// restart (settings-app DESIGN.md step 0).
    PrimeOnScreenMode,
}

fn now_ms32() -> u32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| (d.as_millis() & 0xFFFF_FFFF) as u32)
        .unwrap_or(0)
}

/// Map key name -> `(X11 keysym, X11 keycode = evdev + 8)`.
pub fn lookup_x11_key(name: &str) -> Option<(u32, u32)> {
    if name.len() == 1 {
        let b = name.as_bytes()[0];
        const LETTERS: &[u8; 26] = b"qwertyuiopasdfghjklzxcvbnm";
        const CODES: [u32; 26] = [
            24, 25, 26, 27, 28, 29, 30, 31, 32, 33, 38, 39, 40, 41, 42, 43, 44, 45, 46, 52, 53, 54,
            55, 56, 57, 58,
        ];
        let lower = b.to_ascii_lowercase();
        if let Some(pos) = LETTERS.iter().position(|&c| c == lower) {
            return Some((b as u32, CODES[pos]));
        }
        if b.is_ascii_digit() {
            let code = if b == b'0' { 19 } else { 9 + (b - b'0') as u32 };
            return Some((b as u32, code));
        }
    }
    match name {
        "space" => Some((0x20, 65)),
        "comma" => Some((0x2c, 59)),
        "period" => Some((0x2e, 60)),
        "apostrophe" => Some((0x27, 48)),
        "slash" => Some((0x2f, 61)),
        "minus" => Some((0x2d, 20)),
        "equal" => Some((0x3d, 21)),
        "semicolon" => Some((0x3b, 47)),
        "at" => Some((0x40, 0)),
        "BackSpace" => Some((0xff08, 22)),
        "Tab" => Some((0xff09, 23)),
        "Return" => Some((0xff0d, 36)),
        "Escape" => Some((0xff1b, 9)),
        "Home" => Some((0xff50, 110)),
        "Left" => Some((0xff51, 113)),
        "Up" => Some((0xff52, 111)),
        "Right" => Some((0xff53, 114)),
        "Down" => Some((0xff54, 116)),
        "Page_Up" => Some((0xff55, 112)),
        "Page_Down" => Some((0xff56, 117)),
        "End" => Some((0xff57, 115)),
        "Delete" => Some((0xffff, 119)),
        _ => None,
    }
}

/// Map edit/combo key name -> Wayland evdev keycode (`x11_keycode - 8`).
pub fn lookup_evdev_code(name: &str) -> Option<u32> {
    lookup_x11_key(name).and_then(|(_, x11)| x11.checked_sub(8).filter(|&c| c > 0))
}

struct ImPanelIface {
    ev_tx: mpsc::Sender<FcitxEvent>,
    /// While set, fcitx5's show/hide echoes are our own mode-priming round trip
    /// and must not pop (or hide) the keyboard UI.
    priming: Arc<AtomicBool>,
}

#[zbus::interface(name = "org.fcitx.Fcitx5.VirtualKeyboard1")]
impl ImPanelIface {
    #[zbus(name = "ShowVirtualKeyboard")]
    fn show_virtual_keyboard(&self) {
        if self.priming.load(Ordering::Relaxed) {
            return;
        }
        let _ = self.ev_tx.send(FcitxEvent::ShowRequested { forced: false });
    }

    #[zbus(name = "HideVirtualKeyboard")]
    fn hide_virtual_keyboard(&self) {
        if self.priming.load(Ordering::Relaxed) {
            return;
        }
        let _ = self.ev_tx.send(FcitxEvent::HideRequested { forced: false });
    }

    #[zbus(name = "UpdateCandidateArea")]
    fn update_candidate_area(
        &self,
        mut candidates: Vec<String>,
        has_prev: bool,
        has_next: bool,
        _page_index: i32,
        global_cursor_index: i32,
    ) {
        if candidates.len() > MAX_CANDIDATES {
            candidates.truncate(MAX_CANDIDATES);
        }
        let _ = self.ev_tx.send(FcitxEvent::CandidatesChanged {
            list: candidates,
            has_prev,
            has_next,
            cursor: global_cursor_index,
        });
    }

    #[zbus(name = "UpdatePreeditArea")]
    fn update_preedit_area(&self, preedit: String) {
        let _ = self.ev_tx.send(FcitxEvent::PreeditChanged(preedit));
    }

    #[zbus(name = "UpdatePreeditCaret")]
    fn update_preedit_caret(&self, _caret: i32) {}

    #[zbus(name = "NotifyIMActivated")]
    fn notify_im_activated(&self, unique_name: String) {
        let lang = im_to_lang(&unique_name);
        let _ = self.ev_tx.send(FcitxEvent::LangChanged(lang));
    }

    #[zbus(name = "NotifyIMDeactivated")]
    fn notify_im_deactivated(&self, _unique_name: String) {}

    #[zbus(name = "NotifyIMListChanged")]
    fn notify_im_list_changed(&self) {}
}

struct OskCtrlIface {
    ev_tx: mpsc::Sender<FcitxEvent>,
}

#[zbus::interface(name = "fyi.tb520fu.Osk")]
impl OskCtrlIface {
    #[zbus(name = "show")]
    fn show(&self) {
        let _ = self.ev_tx.send(FcitxEvent::ShowRequested { forced: true });
    }

    #[zbus(name = "hide")]
    fn hide(&self) {
        let _ = self.ev_tx.send(FcitxEvent::HideRequested { forced: true });
    }

    #[zbus(name = "toggle")]
    fn toggle(&self) {
        let _ = self.ev_tx.send(FcitxEvent::ToggleRequested);
    }

    /// Re-prime fcitx5 on-screen keyboard mode (no-op when a physical keyboard is
    /// attached and not folded). Exposed for testing and for the control center.
    #[zbus(name = "prime")]
    fn prime(&self) {
        let _ = self.ev_tx.send(FcitxEvent::PrimeRequested);
    }

    #[zbus(name = "reload_prefs")]
    fn reload_prefs(&self) {
        let _ = self.ev_tx.send(FcitxEvent::ReloadPrefs);
    }
}

struct PuriOskIface {
    ev_tx: mpsc::Sender<FcitxEvent>,
}

#[zbus::interface(name = "sm.puri.OSK0")]
impl PuriOskIface {
    #[zbus(name = "SetVisible")]
    fn set_visible(&self, visible: bool) {
        if visible {
            let _ = self.ev_tx.send(FcitxEvent::ShowRequested { forced: true });
        } else {
            let _ = self.ev_tx.send(FcitxEvent::HideRequested { forced: true });
        }
    }
}

fn im_to_lang(im: &str) -> Lang {
    if im.starts_with("keyboard-") {
        Lang::En
    } else if im == "mozc" || im == "anthy" {
        Lang::Ja
    } else {
        Lang::Zh
    }
}

pub fn spawn_fcitx_bridge(
    preview_mode: bool,
    wakeup_fd: Option<i32>,
    kbd: KbdState,
) -> (tokio::sync::mpsc::UnboundedSender<FcitxCmd>, mpsc::Receiver<FcitxEvent>) {
    let (ev_tx_raw, ev_rx) = mpsc::channel::<FcitxEvent>();
    let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::unbounded_channel::<FcitxCmd>();

    if preview_mode {
        return (cmd_tx, ev_rx);
    }

    // Shared with ImPanelIface: drop fcitx5's show/hide echoes while we prime the
    // on-screen mode, so priming never pops or hides the keyboard UI.
    let priming = Arc::new(AtomicBool::new(false));
    let watcher_tx = cmd_tx.clone();
    let initial_prime_tx = cmd_tx.clone();

    // Wrap sender so every D-Bus event wakes the main poll() loop immediately!
    let ev_tx = ev_tx_raw.clone();
    std::thread::Builder::new()
        .name("tb520fu-osk-dbus".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("tokio runtime");
            rt.block_on(async move {
                let notify_ui = || {
                    if let Some(fd) = wakeup_fd {
                        let one: u64 = 1;
                        unsafe {
                            let _ = libc::write(fd, &one as *const u64 as *const libc::c_void, 8);
                        }
                    }
                };

                let Ok(conn) = Connection::session().await else {
                    eprintln!("tb520fu-osk: session D-Bus unavailable");
                    return;
                };

                // Export impanel, control, and sm.puri.OSK0 compatibility objects
                let (proxy_tx, proxy_rx) = mpsc::channel::<FcitxEvent>();
                let _ = conn
                    .object_server()
                    .at("/org/fcitx/virtualkeyboard/impanel", ImPanelIface { ev_tx: proxy_tx.clone(), priming: priming.clone() })
                    .await;
                let _ = conn
                    .object_server()
                    .at("/Osk", OskCtrlIface { ev_tx: proxy_tx.clone() })
                    .await;
                let _ = conn
                    .object_server()
                    .at("/sm/puri/OSK0", PuriOskIface { ev_tx: proxy_tx })
                    .await;

                // Forward events from object_server -> main UI channel + eventfd wakeup
                let ev_forward = ev_tx.clone();
                std::thread::spawn(move || {
                    let mut mode_echo_until: Option<Instant> = None;
                    let _ = &mut mode_echo_until;
                    while let Ok(ev) = proxy_rx.recv() {
                        let _ = ev_forward.send(ev);
                        if let Some(fd) = wakeup_fd {
                            let one: u64 = 1;
                            unsafe {
                                let _ = libc::write(fd, &one as *const u64 as *const libc::c_void, 8);
                            }
                        }
                    }
                });

                let _ = conn.request_name("org.fcitx.Fcitx5.VirtualKeyboard").await;
                let _ = conn.request_name("fyi.tb520fu.Osk").await;
                let _ = conn.request_name("sm.puri.OSK0").await;

                // Detect preferred Chinese IM ("rime" vs "pinyin") and current IM
                let mut zh_im = detect_chinese_im(&conn).await;
                if let Ok(reply) = conn
                    .call_method(
                        Some("org.fcitx.Fcitx5"),
                        "/controller",
                        Some("org.fcitx.Fcitx.Controller1"),
                        "CurrentInputMethod",
                        &(),
                    )
                    .await
                {
                    if let Ok(cur) = reply.body().deserialize::<String>() {
                        if !cur.is_empty() {
                            if !cur.starts_with("keyboard-") && cur != "mozc" {
                                zh_im = cur.clone();
                            }
                            let _ = ev_tx.send(FcitxEvent::LangChanged(im_to_lang(&cur)));
                            notify_ui();
                        }
                    }
                }

                // fcitx5 resets InputMethodMode to PhysicalKeyboard on every (re)start.
                // Watch its bus name and re-prime whenever a new owner appears.
                {
                    let conn_w = conn.clone();
                    tokio::spawn(async move {
                        let Ok(rule) = zbus::MatchRule::builder()
                            .msg_type(zbus::message::Type::Signal)
                            .interface("org.freedesktop.DBus")
                            .and_then(|b| b.member("NameOwnerChanged"))
                            .map(|b| b.build())
                        else {
                            return;
                        };
                        let Ok(mut stream) =
                            zbus::MessageStream::for_match_rule(rule, &conn_w, Some(16)).await
                        else {
                            return;
                        };
                        while let Some(msg) = next_bus_msg(&mut stream).await {
                            if let Ok((name, _old, new)) =
                                msg.body().deserialize::<(String, String, String)>()
                            {
                                if name == "org.fcitx.Fcitx5" && !new.is_empty() {
                                    // Give fcitx5 a moment to load its UI addons.
                                    tokio::time::sleep(Duration::from_millis(1000)).await;
                                    let _ = watcher_tx.send(FcitxCmd::PrimeOnScreenMode);
                                }
                            }
                        }
                    });
                }
                // Initial prime: covers "keyboard already detached / folded at boot"
                // and "fcitx5 started before us". No-op when it is not needed.
                let _ = initial_prime_tx.send(FcitxCmd::PrimeOnScreenMode);

                while let Some(cmd) = cmd_rx.recv().await {
                    match cmd {
                        FcitxCmd::PrimeOnScreenMode => {
                            prime_on_screen_mode(&conn, &priming, &kbd).await;
                        }
                        FcitxCmd::EnterOnScreenMode => {
                            let _ = conn
                                .call_method(
                                    Some("org.fcitx.Fcitx5"),
                                    "/virtualkeyboard",
                                    Some("org.fcitx.Fcitx.VirtualKeyboard1"),
                                    "ShowVirtualKeyboard",
                                    &(),
                                )
                                .await;
                        }
                        FcitxCmd::RequestHide => {
                            let _ = conn
                                .call_method(
                                    Some("org.fcitx.Fcitx5"),
                                    "/virtualkeyboard",
                                    Some("org.fcitx.Fcitx.VirtualKeyboard1"),
                                    "HideVirtualKeyboard",
                                    &(),
                                )
                                .await;
                        }
                        FcitxCmd::ClearComposition => {
                            process_key_with_retry(&conn, 0xff1b, 9, 0, false).await;
                            process_key_with_retry(&conn, 0xff1b, 9, 0, true).await;
                        }
                        FcitxCmd::TypeKey(name) => {
                            if let Some((sym, code)) = lookup_x11_key(&name) {
                                let state = if (b'A'..=b'Z').contains(&(sym as u8)) { 1u32 } else { 0u32 };
                                process_key_with_retry(&conn, sym, code, state, false).await;
                                process_key_with_retry(&conn, sym, code, state, true).await;
                            }
                        }
                        FcitxCmd::CommitString(s) => {
                            if s.is_empty() {
                                continue;
                            }
                            // 1. Try fcitx5 OskCommit1 addon (/y700osk) first
                            let addon_ok = conn
                                .call_method(
                                    Some("org.fcitx.Fcitx5"),
                                    "/y700osk",
                                    Some("fyi.y700.OskCommit1"),
                                    "CommitString",
                                    &(s.as_str(),),
                                )
                                .await
                                .is_ok();
                            if addon_ok {
                                continue;
                            }
                            // 2. Pure printable ASCII -> type directly through VirtualKeyboardBackend1
                            if s.bytes().all(|b| (0x20..0x7f).contains(&b)) {
                                for b in s.bytes() {
                                    process_key_with_retry(&conn, b as u32, 0, 0, false).await;
                                    process_key_with_retry(&conn, b as u32, 0, 0, true).await;
                                }
                            } else {
                                // 3. Fallback for CJK symbols / Emoji / Clipboard when addon not yet installed:
                                // copy via wl-copy and ask UI thread to inject Ctrl+V via zwp_virtual_keyboard_v1!
                                let _ = ev_tx.send(FcitxEvent::FallbackPasteCommit(s));
                                notify_ui();
                            }
                        }
                        FcitxCmd::SelectCandidate(idx) => {
                            let res = conn
                                .call_method(
                                    Some("org.fcitx.Fcitx5"),
                                    "/virtualkeyboard",
                                    Some("org.fcitx.Fcitx5.VirtualKeyboardBackend1"),
                                    "SelectCandidate",
                                    &(idx,),
                                )
                                .await;
                            if res.is_err() {
                                let _ = conn
                                    .call_method(
                                        Some("org.fcitx.Fcitx5"),
                                        "/virtualkeyboard",
                                        Some("org.fcitx.Fcitx.VirtualKeyboard1"),
                                        "ShowVirtualKeyboard",
                                        &(),
                                    )
                                    .await;
                                let _ = conn
                                    .call_method(
                                        Some("org.fcitx.Fcitx5"),
                                        "/virtualkeyboard",
                                        Some("org.fcitx.Fcitx5.VirtualKeyboardBackend1"),
                                        "SelectCandidate",
                                        &(idx,),
                                    )
                                    .await;
                            }
                        }
                        FcitxCmd::Page { next } => {
                            let member = if next { "NextPage" } else { "PrevPage" };
                            let _ = conn
                                .call_method(
                                    Some("org.fcitx.Fcitx5"),
                                    "/virtualkeyboard",
                                    Some("org.fcitx.Fcitx5.VirtualKeyboardBackend1"),
                                    member,
                                    &(),
                                )
                                .await;
                        }
                        FcitxCmd::SetLang(lang) => {
                            if lang == Lang::Zh {
                                zh_im = detect_chinese_im(&conn).await;
                            }
                            let target = match lang {
                                Lang::En => "keyboard-us",
                                Lang::Ja => "mozc",
                                Lang::Zh => zh_im.as_str(),
                            };
                            let _ = conn
                                .call_method(
                                    Some("org.fcitx.Fcitx5"),
                                    "/controller",
                                    Some("org.fcitx.Fcitx.Controller1"),
                                    "SetCurrentIM",
                                    &(target,),
                                )
                                .await;
                        }
                    }
                }
                let _ = Duration::from_millis(1);
            });
        })
        .ok();

    (cmd_tx, ev_rx)
}

/// Drive fcitx5 into `InputMethodMode::OnScreenKeyboard` without changing the OSK
/// UI state. fcitx5 only auto-pops the OSK on input focus while in that mode
/// (fcitx5 5.1.12 `src/ui/virtualkeyboard/virtualkeyboard.cpp`); a physical key
/// press drops it back to `PhysicalKeyboard`. The only way in is a
/// `ShowVirtualKeyboard` round trip, so we show then immediately hide, suppressing
/// both echoes via the `priming` flag. `HideVirtualKeyboard` keeps the mode because
/// fcitx5's `virtualKeyboardAutoShow` defaults to true.
async fn prime_on_screen_mode(conn: &Connection, priming: &Arc<AtomicBool>, kbd: &KbdState) {
    if !kbd.should_prime() {
        eprintln!("tb520fu-osk: prime skipped (pogo keyboard attached, not in tablet mode)");
        return;
    }
    if priming.load(Ordering::Relaxed) {
        return;
    }
    let Ok(dbus) = zbus::fdo::DBusProxy::new(conn).await else {
        return;
    };
    let Ok(name) = zbus::names::BusName::try_from("org.fcitx.Fcitx5") else {
        return;
    };
    if !matches!(dbus.name_has_owner(name).await, Ok(true)) {
        eprintln!("tb520fu-osk: prime skipped (fcitx5 not on the bus yet)");
        return;
    }

    priming.store(true, Ordering::Relaxed);
    eprintln!("tb520fu-osk: priming fcitx5 on-screen keyboard mode");
    let call = |member: &'static str| {
        conn.call_method(
            Some("org.fcitx.Fcitx5"),
            "/virtualkeyboard",
            Some("org.fcitx.Fcitx.VirtualKeyboard1"),
            member,
            &(),
        )
    };
    let mut ok = matches!(
        tokio::time::timeout(Duration::from_secs(2), call("ShowVirtualKeyboard")).await,
        Ok(Ok(_))
    );
    if !ok {
        // fcitx5 may still be initializing its virtualkeyboard addon; retry once.
        tokio::time::sleep(Duration::from_millis(500)).await;
        ok = matches!(
            tokio::time::timeout(Duration::from_secs(2), call("ShowVirtualKeyboard")).await,
            Ok(Ok(_))
        );
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    let _ = tokio::time::timeout(Duration::from_secs(2), call("HideVirtualKeyboard")).await;
    // Let any in-flight echoes drain while suppression is still active.
    tokio::time::sleep(Duration::from_millis(250)).await;
    priming.store(false, Ordering::Relaxed);
    if ok {
        eprintln!("tb520fu-osk: fcitx5 on-screen keyboard mode primed");
    } else {
        eprintln!("tb520fu-osk: prime failed (ShowVirtualKeyboard error)");
    }
}

/// Drain a `zbus::MessageStream` without a `futures-util` dependency (same pattern
/// as the shell's `sys_state.rs`).
async fn next_bus_msg(stream: &mut zbus::MessageStream) -> Option<zbus::Message> {
    loop {
        let item = std::future::poll_fn(|cx| {
            zbus::export::futures_core::Stream::poll_next(std::pin::Pin::new(&mut *stream), cx)
        })
        .await?;
        if let Ok(msg) = item {
            return Some(msg);
        }
    }
}

async fn process_key_with_retry(conn: &Connection, sym: u32, code: u32, state: u32, release: bool) {
    if sym == 0 && code == 0 {
        return;
    }
    let res = conn
        .call_method(
            Some("org.fcitx.Fcitx5"),
            "/virtualkeyboard",
            Some("org.fcitx.Fcitx5.VirtualKeyboardBackend1"),
            "ProcessKeyEvent",
            &(sym, code, state, release, now_ms32()),
        )
        .await;
    if res.is_err() {
        let _ = conn
            .call_method(
                Some("org.fcitx.Fcitx5"),
                "/virtualkeyboard",
                Some("org.fcitx.Fcitx.VirtualKeyboard1"),
                "ShowVirtualKeyboard",
                &(),
            )
            .await;
        let _ = conn
            .call_method(
                Some("org.fcitx.Fcitx5"),
                "/virtualkeyboard",
                Some("org.fcitx.Fcitx5.VirtualKeyboardBackend1"),
                "ProcessKeyEvent",
                &(sym, code, state, release, now_ms32()),
            )
            .await;
    }
}

/// Automatically prefer whichever Chinese engine (`rime`, `pinyin`, `shuangpin`) is configured in fcitx5!
async fn detect_chinese_im(conn: &Connection) -> String {
    // Check CurrentInputMethod group entries via AvailableInputMethods or try rime/pinyin
    if let Ok(reply) = conn
        .call_method(
            Some("org.fcitx.Fcitx5"),
            "/controller",
            Some("org.fcitx.Fcitx.Controller1"),
            "AvailableInputMethods",
            &(),
        )
        .await
    {
        type AvailRow = (String, String, String, String, String, bool);
        if let Ok(rows) = reply.body().deserialize::<Vec<AvailRow>>() {
            for pref in ["rime", "pinyin", "shuangpin"] {
                if rows.iter().any(|r| r.0 == pref) {
                    return pref.to_string();
                }
            }
        }
    }
    "pinyin".to_string()
}

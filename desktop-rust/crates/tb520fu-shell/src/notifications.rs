// SPDX-License-Identifier: MIT
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use cosmic_text::Align;
use tb520fu_ui::canvas::{Canvas, RectF};
use tb520fu_ui::color::{Palette, Rgba};
use tb520fu_ui::spring::Spring1D;
use tb520fu_ui::svg_icons::IconSet;
use tb520fu_ui::text::{FontWeight, TextEngine};
use zbus::zvariant::Value;

use crate::sys_state::format_local_clock;

#[derive(Debug, Clone)]
pub struct NotifItem {
    pub id: u32,
    pub app_name: String,
    pub summary: String,
    pub body: String,
    pub time_hm: String,
    pub created_at: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HwOsdCmd {
    VolumeDelta(i8),
    ToggleMute,
    ToggleMicMute,
    BrightnessDelta(i8),
}

#[derive(Debug, Clone)]
pub enum ShellBusEvent {
    NotifAdded(NotifItem),
    NotifClosed(u32),
    HardwareKeyOsd(HwOsdCmd),
    LockRequested,
    UnlockRequested,
    StartStandby,
    ToggleControlCenter,
    ToggleNotifications,
    StartAnnot,
    ToggleQuickNote,
    OpenWindowMenu {
        win_id: u64,
        cx: f32,
        ty: f32,
        is_floating: bool,
    },
    CurtainSwipeBegin,
    CurtainSwipeUpdate(f32),
    CurtainSwipeEnd,
    TouchpadEdgePull {
        wing: crate::statusbar::PullWing,
        frac: f32,
        velocity: f32,
        finished: bool,
    },
    TouchpadLockWake,
    SetPref {
        key: String,
        value_json: String,
    },
    SystemResumed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BannerAction {
    None,
    Dismiss,
    OpenNotifications,
}

static NEXT_NOTIF_ID: AtomicU32 = AtomicU32::new(1);

struct FreedesktopNotifIface {
    tx: mpsc::Sender<ShellBusEvent>,
    wakeup_fd: Option<i32>,
}

impl FreedesktopNotifIface {
    fn wake(&self) {
        if let Some(fd) = self.wakeup_fd {
            let one: u64 = 1;
            unsafe {
                let _ = libc::write(fd, &one as *const u64 as *const libc::c_void, 8);
            }
        }
    }
}

#[zbus::interface(name = "org.freedesktop.Notifications")]
impl FreedesktopNotifIface {
    #[zbus(name = "GetCapabilities")]
    fn get_capabilities(&self) -> Vec<String> {
        vec!["body".into(), "persistence".into()]
    }

    #[zbus(name = "GetServerInformation")]
    fn get_server_information(&self) -> (String, String, String, String) {
        (
            "tb520fu-shell".into(),
            "TB520FU".into(),
            "0.1.0".into(),
            "1.2".into(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    #[zbus(name = "Notify")]
    fn notify(
        &self,
        app_name: String,
        replaces_id: u32,
        _app_icon: String,
        summary: String,
        body: String,
        _actions: Vec<String>,
        _hints: HashMap<String, Value<'_>>,
        _expire_timeout: i32,
    ) -> u32 {
        let id = if replaces_id != 0 {
            replaces_id
        } else {
            NEXT_NOTIF_ID.fetch_add(1, Ordering::Relaxed)
        };
        let (hm, _, _) = format_local_clock();
        let item = NotifItem {
            id,
            app_name: if app_name.is_empty() {
                "系统通知".into()
            } else {
                app_name
            },
            summary,
            body,
            time_hm: hm,
            created_at: Instant::now(),
        };
        let _ = self.tx.send(ShellBusEvent::NotifAdded(item));
        self.wake();
        id
    }

    #[zbus(name = "CloseNotification")]
    fn close_notification(&self, id: u32) {
        let _ = self.tx.send(ShellBusEvent::NotifClosed(id));
        self.wake();
    }
}

struct ShellCtrlIface {
    tx: mpsc::Sender<ShellBusEvent>,
    wakeup_fd: Option<i32>,
}

impl ShellCtrlIface {
    fn wake(&self) {
        if let Some(fd) = self.wakeup_fd {
            let one: u64 = 1;
            unsafe {
                let _ = libc::write(fd, &one as *const u64 as *const libc::c_void, 8);
            }
        }
    }
}

#[zbus::interface(name = "fyi.tb520fu.Shell")]
impl ShellCtrlIface {
    #[zbus(name = "Lock")]
    fn lock(&self) {
        let _ = self.tx.send(ShellBusEvent::LockRequested);
        self.wake();
    }

    #[zbus(name = "Unlock")]
    fn unlock(&self) {
        let _ = self.tx.send(ShellBusEvent::UnlockRequested);
        self.wake();
    }

    #[zbus(name = "StartStandby")]
    fn start_standby(&self) {
        let _ = self.tx.send(ShellBusEvent::StartStandby);
        self.wake();
    }

    #[zbus(name = "ToggleControlCenter")]
    fn toggle_control_center(&self) {
        let _ = self.tx.send(ShellBusEvent::ToggleControlCenter);
        self.wake();
    }

    #[zbus(name = "ToggleNotifications")]
    fn toggle_notifications(&self) {
        let _ = self.tx.send(ShellBusEvent::ToggleNotifications);
        self.wake();
    }

    #[zbus(name = "StartAnnot")]
    fn start_annot(&self) {
        let _ = self.tx.send(ShellBusEvent::StartAnnot);
        self.wake();
    }

    #[zbus(name = "ToggleQuickNote")]
    fn toggle_quick_note(&self) {
        let _ = self.tx.send(ShellBusEvent::ToggleQuickNote);
        self.wake();
    }

    #[zbus(name = "GetPrefs")]
    fn get_prefs(&self) -> String {
        crate::sys_state::cached_prefs_json()
    }

    #[zbus(name = "SetPref")]
    fn set_pref(&self, key: String, value_json: String) -> bool {
        if !crate::sys_state::validate_and_stage_set_pref(&key, &value_json) {
            return false;
        }
        let _ = self.tx.send(ShellBusEvent::SetPref { key, value_json });
        self.wake();
        true
    }

    #[zbus(signal, name = "PrefsChanged")]
    async fn prefs_changed(
        emitter: &zbus::object_server::SignalEmitter<'_>,
        key: &str,
        value_json: &str,
    ) -> zbus::Result<()>;
}

/// Register `org.freedesktop.Notifications` and `fyi.tb520fu.Shell` on the shared session `zbus::Connection`.
pub(crate) async fn register_session_objects(
    conn: &zbus::Connection,
    tx: mpsc::Sender<ShellBusEvent>,
    wakeup_fd: Option<i32>,
) {
    let _ = conn
        .object_server()
        .at(
            "/org/freedesktop/Notifications",
            FreedesktopNotifIface {
                tx: tx.clone(),
                wakeup_fd,
            },
        )
        .await;
    let _ = conn
        .object_server()
        .at(
            "/Shell",
            ShellCtrlIface { tx, wakeup_fd },
        )
        .await;
    let _ = conn.request_name("org.freedesktop.Notifications").await;
    let _ = conn.request_name("fyi.tb520fu.Shell").await;
}

pub(crate) fn decode_logind_signal(member: &str) -> Option<ShellBusEvent> {
    match member {
        "Lock" => Some(ShellBusEvent::LockRequested),
        "Unlock" => Some(ShellBusEvent::UnlockRequested),
        _ => None,
    }
}

/// Floating incoming Notification Banner (`460 x 104` surface anchored below TopBar at `margin_top = 52`).
/// Hardware Volume / Brightness / Mute OSD is fused directly into the TopBar center Status Capsule (`StatusBar`).
pub struct BannerOsdView {
    pub active_banner: Option<NotifItem>,
    pub expires_at: Option<Instant>,
    pub reveal: Spring1D,
    drag_slot: Option<i32>,
    drag_y0: f32,
    drag_dy: f32,
    card_rect: RectF,
}

impl Default for BannerOsdView {
    fn default() -> Self {
        Self {
            active_banner: None,
            expires_at: None,
            reveal: Spring1D::normalized(0.0, 24.0, 0.96),
            drag_slot: None,
            drag_y0: 0.0,
            drag_dy: 0.0,
            card_rect: RectF::new(14.0, 6.0, 432.0, 84.0),
        }
    }
}

impl BannerOsdView {
    pub fn push_banner(&mut self, item: NotifItem) {
        self.push_banner_with_timeout(item, 4200);
    }

    pub fn push_banner_with_timeout(&mut self, item: NotifItem, timeout_ms: u32) {
        self.active_banner = Some(item);
        self.expires_at =
            Some(Instant::now() + Duration::from_millis(timeout_ms.clamp(1500, 15000) as u64));
        self.drag_slot = None;
        self.drag_dy = 0.0;
        self.reveal.set_target(1.0);
    }

    pub fn dismiss(&mut self) {
        self.expires_at = None;
        self.drag_slot = None;
        self.reveal.set_target(0.0);
    }

    pub fn is_animating(&self) -> bool {
        self.reveal.is_animating()
    }

    pub fn is_visible(&self) -> bool {
        self.active_banner.is_some() && (self.reveal.pos > 0.004 || self.reveal.target > 0.0)
    }

    pub fn step(&mut self, dt: f32) -> bool {
        let mut changed = false;
        if self.drag_slot.is_none() {
            if let Some(exp) = self.expires_at {
                if Instant::now() >= exp {
                    self.expires_at = None;
                    self.reveal.set_target(0.0);
                    changed = true;
                }
            }
        }
        if self.reveal.step(dt) {
            changed = true;
        }
        if self.reveal.target == 0.0 && self.reveal.pos <= 0.006 && !self.reveal.is_animating() {
            if self.active_banner.take().is_some() {
                self.drag_dy = 0.0;
                changed = true;
            }
        }
        changed
    }

    pub fn on_touch_down(&mut self, slot: i32, x: f32, y: f32) -> bool {
        if !self.is_visible() || self.reveal.pos < 0.35 {
            return false;
        }
        if self.card_rect.inset(-6.0, -6.0).contains(x, y) {
            self.drag_slot = Some(slot);
            self.drag_y0 = y;
            self.drag_dy = 0.0;
            self.expires_at = None;
            return true;
        }
        false
    }

    pub fn on_touch_motion(&mut self, slot: i32, _x: f32, y: f32) -> bool {
        if self.drag_slot != Some(slot) {
            return false;
        }
        let raw_dy = y - self.drag_y0;
        self.drag_dy = if raw_dy < 0.0 {
            raw_dy.max(-78.0)
        } else {
            (raw_dy * 0.22).min(14.0)
        };
        true
    }

    pub fn on_touch_up(&mut self, slot: i32, _x: f32, y: f32) -> BannerAction {
        if self.drag_slot != Some(slot) {
            return BannerAction::None;
        }
        self.drag_slot = None;
        let dy = y - self.drag_y0;
        if dy < -18.0 {
            self.dismiss();
            BannerAction::Dismiss
        } else if dy.abs() <= 14.0 {
            self.dismiss();
            self.drag_dy = 0.0;
            BannerAction::OpenNotifications
        } else {
            self.drag_dy = 0.0;
            self.expires_at = Some(Instant::now() + Duration::from_millis(2400));
            BannerAction::None
        }
    }

    pub fn on_touch_cancel(&mut self) {
        if self.drag_slot.take().is_some() {
            self.drag_dy = 0.0;
            self.expires_at = Some(Instant::now() + Duration::from_millis(2400));
        }
    }

    /// Render into a `460 x 104` logical overlay surface anchored below TopBar (`margin_top = 52`).
    pub fn render(
        &mut self,
        canvas: &mut Canvas,
        icons: &IconSet,
        text: &mut TextEngine,
        pal: &Palette,
    ) -> Vec<RectF> {
        canvas.clear(Rgba::TRANSPARENT);
        let p = self.reveal.pos.clamp(0.0, 1.0);
        let Some(notif) = &self.active_banner else {
            return Vec::new();
        };
        if p <= 0.004 {
            return Vec::new();
        }

        let w = canvas.logical_width();
        let cw = 432.0_f32;
        let ch = 84.0_f32;
        let y_off = (1.0 - p) * -26.0 + self.drag_dy;
        let alpha = (p * (1.0 - (-self.drag_dy / 74.0).clamp(0.0, 0.88))).clamp(0.0, 1.0);
        let card = RectF::new((w - cw) * 0.5, (6.0 + y_off).max(-72.0), cw, ch);
        self.card_rect = card;

        canvas.draw_glass_shadow(card, 22.0, 10.0, 3.0, 0.28 * alpha);
        if pal.glass {
            canvas.fill_glass_corners_styled(
                card,
                [22.0; 4],
                pal,
                pal.glass_tint,
                Rgba::TRANSPARENT,
                0.04,
                alpha,
            );
        } else {
            canvas.fill_round_rect(card, 22.0, pal.card.with_alpha(alpha));
        }

        let (c1, c2, ic_name) = crate::curtain::notif_app_style(&notif.app_name);
        let badge = RectF::new(card.x + 14.0, card.y + (ch - 40.0) * 0.5, 40.0, 40.0);
        canvas.fill_v_gradient(badge, 12.0, c1.with_alpha(alpha), c2.with_alpha(alpha));
        icons.draw(
            canvas,
            ic_name,
            RectF::new(badge.x + 10.0, badge.y + 10.0, 20.0, 20.0),
            Rgba::WHITE.with_alpha(alpha),
            2.0,
        );

        let tx = badge.x + badge.w + 12.0;
        let avail_w = (card.w - (tx - card.x) - 16.0).max(80.0);
        text.draw_in_rect(
            canvas,
            &notif.app_name,
            RectF::new(tx, card.y + 11.0, avail_w - 58.0, 18.0),
            12.0,
            FontWeight::Medium,
            pal.ink3.with_alpha(alpha),
            Align::Left,
        );
        text.draw_in_rect(
            canvas,
            &notif.time_hm,
            RectF::new(card.x + card.w - 72.0, card.y + 11.0, 56.0, 18.0),
            11.6,
            FontWeight::Normal,
            pal.ink2.with_alpha(alpha * 0.88),
            Align::Right,
        );

        let has_body = !notif.body.trim().is_empty();
        text.draw_in_rect(
            canvas,
            &notif.summary,
            RectF::new(tx, if has_body { card.y + 30.0 } else { card.y + 36.0 }, avail_w, 22.0),
            14.2,
            FontWeight::SemiBold,
            pal.ink.with_alpha(alpha),
            Align::Left,
        );
        if has_body {
            text.draw_in_rect(
                canvas,
                &notif.body,
                RectF::new(tx, card.y + 51.0, avail_w, 20.0),
                12.8,
                FontWeight::Normal,
                pal.ink2.with_alpha(alpha * 0.92),
                Align::Left,
            );
        }

        // Subtle upward-dismiss pill hint at bottom center
        canvas.fill_round_rect(
            RectF::new(card.x + (cw - 28.0) * 0.5, card.y + ch - 5.5, 28.0, 2.6),
            1.3,
            pal.ink2.with_alpha(0.24 * alpha),
        );

        if p > 0.35 {
            vec![card]
        } else {
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tb520fu_ui::color::Skin;

    #[test]
    fn banner_spring_supports_swipe_up_dismiss_and_tap_open() {
        let mut banner = BannerOsdView::default();
        let mut canvas = Canvas::new(920, 208, 2.0);
        let icons = IconSet::new();
        let mut text = TextEngine::new();
        let pal = Palette::for_skin(Skin::GlassDark);

        banner.push_banner(NotifItem {
            id: 1,
            app_name: "系统通知".into(),
            summary: "手写笔已吸附".into(),
            body: "电量 98%".into(),
            time_hm: "20:15".into(),
            created_at: Instant::now(),
        });
        for _ in 0..45 {
            banner.step(1.0 / 120.0);
        }
        let regions = banner.render(&mut canvas, &icons, &mut text, &pal);
        assert_eq!(regions.len(), 1);

        // Tap opens Notification Wing
        banner.on_touch_down(0, 230.0, 50.0);
        assert_eq!(
            banner.on_touch_up(0, 232.0, 51.0),
            BannerAction::OpenNotifications
        );

        // Swipe-up dismisses cleanly
        banner.push_banner(NotifItem {
            id: 2,
            app_name: "日历".into(),
            summary: "产品评审".into(),
            body: "10 分钟后开始".into(),
            time_hm: "20:20".into(),
            created_at: Instant::now(),
        });
        for _ in 0..45 {
            banner.step(1.0 / 120.0);
        }
        let _ = banner.render(&mut canvas, &icons, &mut text, &pal);
        banner.on_touch_down(1, 230.0, 68.0);
        banner.on_touch_motion(1, 230.0, 18.0);
        assert_eq!(banner.on_touch_up(1, 230.0, 18.0), BannerAction::Dismiss);
    }
}

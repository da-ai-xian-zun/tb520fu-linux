// SPDX-License-Identifier: MIT
use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::sync::mpsc;
use std::time::Duration;

use wayland_client::protocol::{wl_registry, wl_seat};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols_wlr::data_control::v1::client::{
    zwlr_data_control_device_v1, zwlr_data_control_manager_v1, zwlr_data_control_offer_v1,
    zwlr_data_control_source_v1,
};

pub enum ClipboardCmd {
    SetText(String),
    SetTextAck(String, mpsc::SyncSender<bool>),
}

#[derive(Clone)]
pub struct ClipboardCtl {
    tx: mpsc::Sender<ClipboardCmd>,
    cmd_wake_fd: i32,
}

impl ClipboardCtl {
    pub fn set_text(&self, text: String) -> bool {
        let ok = self.tx.send(ClipboardCmd::SetText(text)).is_ok();
        if ok && self.cmd_wake_fd >= 0 {
            let one: u64 = 1;
            unsafe {
                let _ = libc::write(
                    self.cmd_wake_fd,
                    &one as *const u64 as *const libc::c_void,
                    8,
                );
            }
        }
        ok
    }

    /// Set clipboard selection via `zwlr-data-control-v1` and wait up to `8 ms` for flush ack.
    pub fn set_text_ack(&self, text: String) -> bool {
        let (ack_tx, ack_rx) = mpsc::sync_channel(1);
        if self.tx.send(ClipboardCmd::SetTextAck(text, ack_tx)).is_err() {
            return false;
        }
        if self.cmd_wake_fd >= 0 {
            let one: u64 = 1;
            unsafe {
                let _ = libc::write(
                    self.cmd_wake_fd,
                    &one as *const u64 as *const libc::c_void,
                    8,
                );
            }
        }
        ack_rx
            .recv_timeout(Duration::from_millis(8))
            .unwrap_or(false)
    }
}

struct OfferEntry {
    has_utf8: bool,
    has_plain: bool,
}

struct ClipWlState {
    mgr: Option<zwlr_data_control_manager_v1::ZwlrDataControlManagerV1>,
    seat: Option<wl_seat::WlSeat>,
    dev: Option<zwlr_data_control_device_v1::ZwlrDataControlDeviceV1>,
    active_source: Option<zwlr_data_control_source_v1::ZwlrDataControlSourceV1>,
    active_text: String,
    offers: HashMap<u32, (zwlr_data_control_offer_v1::ZwlrDataControlOfferV1, OfferEntry)>,
    pending_read: Option<(zwlr_data_control_offer_v1::ZwlrDataControlOfferV1, &'static str)>,
}

impl Dispatch<wl_registry::WlRegistry, ()> for ClipWlState {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "zwlr_data_control_manager_v1" => {
                    state.mgr = Some(registry.bind(name, version.min(2), qh, ()));
                }
                "wl_seat" if state.seat.is_none() => {
                    state.seat = Some(registry.bind(name, version.min(7), qh, ()));
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for ClipWlState {
    fn event(
        _: &mut Self,
        _: &wl_seat::WlSeat,
        _: wl_seat::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<zwlr_data_control_manager_v1::ZwlrDataControlManagerV1, ()> for ClipWlState {
    fn event(
        _: &mut Self,
        _: &zwlr_data_control_manager_v1::ZwlrDataControlManagerV1,
        _: zwlr_data_control_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<zwlr_data_control_device_v1::ZwlrDataControlDeviceV1, ()> for ClipWlState {
    fn event(
        state: &mut Self,
        _: &zwlr_data_control_device_v1::ZwlrDataControlDeviceV1,
        event: zwlr_data_control_device_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_data_control_device_v1::Event::DataOffer { id } => {
                let oid = id.id().protocol_id();
                state.offers.insert(
                    oid,
                    (
                        id,
                        OfferEntry {
                            has_utf8: false,
                            has_plain: false,
                        },
                    ),
                );
            }
            zwlr_data_control_device_v1::Event::Selection { id } => {
                let Some(offer) = id else { return };
                let oid = offer.id().protocol_id();
                if let Some((obj, info)) = state.offers.remove(&oid) {
                    let mime = if info.has_utf8 {
                        Some("text/plain;charset=utf-8")
                    } else if info.has_plain {
                        Some("text/plain")
                    } else {
                        None
                    };
                    if let Some(m) = mime {
                        state.pending_read = Some((obj, m));
                    } else {
                        obj.destroy();
                    }
                }
                // Clean up any stale unselected offers
                for (_, (stale, _)) in state.offers.drain() {
                    stale.destroy();
                }
            }
            _ => {}
        }
    }

    wayland_client::event_created_child!(ClipWlState, zwlr_data_control_device_v1::ZwlrDataControlDeviceV1, [
        zwlr_data_control_device_v1::EVT_DATA_OFFER_OPCODE => (zwlr_data_control_offer_v1::ZwlrDataControlOfferV1, ()),
    ]);
}

impl Dispatch<zwlr_data_control_offer_v1::ZwlrDataControlOfferV1, ()> for ClipWlState {
    fn event(
        state: &mut Self,
        proxy: &zwlr_data_control_offer_v1::ZwlrDataControlOfferV1,
        event: zwlr_data_control_offer_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwlr_data_control_offer_v1::Event::Offer { mime_type } = event {
            if let Some((_, entry)) = state.offers.get_mut(&proxy.id().protocol_id()) {
                if mime_type == "text/plain;charset=utf-8" || mime_type == "UTF8_STRING" {
                    entry.has_utf8 = true;
                } else if mime_type == "text/plain" || mime_type == "STRING" {
                    entry.has_plain = true;
                }
            }
        }
    }
}

impl Dispatch<zwlr_data_control_source_v1::ZwlrDataControlSourceV1, ()> for ClipWlState {
    fn event(
        state: &mut Self,
        proxy: &zwlr_data_control_source_v1::ZwlrDataControlSourceV1,
        event: zwlr_data_control_source_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_data_control_source_v1::Event::Send { fd, .. } => {
                let bytes = state.active_text.clone().into_bytes();
                std::thread::spawn(move || {
                    let mut f = std::fs::File::from(fd);
                    let _ = f.write_all(&bytes);
                });
            }
            zwlr_data_control_source_v1::Event::Cancelled => {
                proxy.destroy();
                if state
                    .active_source
                    .as_ref()
                    .is_some_and(|s| s.id() == proxy.id())
                {
                    state.active_source = None;
                }
            }
            _ => {}
        }
    }
}

/// Spawn a zero-polling native `zwlr_data_control_manager_v1` Wayland clipboard client!
pub fn spawn_native_clipboard(
    on_text: mpsc::Sender<String>,
    wakeup_fd: Option<i32>,
) -> ClipboardCtl {
    let (cmd_tx, cmd_rx) = mpsc::channel::<ClipboardCmd>();
    let cmd_wake_fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
    std::thread::Builder::new()
        .name("tb520fu-data-control".into())
        .spawn(move || {
            let Ok(conn) = Connection::connect_to_env() else {
                return;
            };
            let mut queue = conn.new_event_queue();
            let qh = queue.handle();
            let mut state = ClipWlState {
                mgr: None,
                seat: None,
                dev: None,
                active_source: None,
                active_text: String::new(),
                offers: HashMap::new(),
                pending_read: None,
            };
            let _reg = conn.display().get_registry(&qh, ());
            if queue.roundtrip(&mut state).is_err() {
                return;
            }
            let (Some(mgr), Some(seat)) = (&state.mgr, &state.seat) else {
                return;
            };
            state.dev = Some(mgr.get_data_device(seat, &qh, ()));
            let _ = queue.roundtrip(&mut state);

            let mut last_sent = String::new();
            loop {
                // 1. Drain outgoing SetText requests
                while let Ok(cmd) = cmd_rx.try_recv() {
                    let (text, ack_opt) = match cmd {
                        ClipboardCmd::SetText(t) => (t, None),
                        ClipboardCmd::SetTextAck(t, ack) => (t, Some(ack)),
                    };
                    if text.is_empty() {
                        if let Some(ack) = ack_opt {
                            let _ = ack.send(false);
                        }
                        continue;
                    }
                    let mut ok = false;
                    if let (Some(mgr), Some(dev)) = (&state.mgr, &state.dev) {
                        if let Some(old) = state.active_source.take() {
                            old.destroy();
                        }
                        let src = mgr.create_data_source(&qh, ());
                        src.offer("text/plain;charset=utf-8".into());
                        src.offer("text/plain".into());
                        src.offer("UTF8_STRING".into());
                        dev.set_selection(Some(&src));
                        state.active_text = text.clone();
                        last_sent = text;
                        state.active_source = Some(src);
                        ok = conn.flush().is_ok();
                    }
                    if let Some(ack) = ack_opt {
                        let _ = ack.send(ok);
                    }
                }

                // 2. Drain incoming selection offers cleanly via pipe
                if let Some((offer, mime)) = state.pending_read.take() {
                    let mut fds = [0i32; 2];
                    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } == 0 {
                        let read_fd = unsafe { OwnedFd::from_raw_fd(fds[0]) };
                        let write_fd = unsafe { OwnedFd::from_raw_fd(fds[1]) };
                        offer.receive(mime.to_string(), write_fd.as_fd());
                        let _ = conn.flush();
                        drop(write_fd);
                        offer.destroy();

                        let mut f = std::fs::File::from(read_fd);
                        let mut buf = Vec::with_capacity(512);
                        let _ = (&mut f).take(65536).read_to_end(&mut buf);
                        if let Ok(s) = String::from_utf8(buf) {
                            let trimmed = s.trim().to_string();
                            if !trimmed.is_empty() && trimmed != last_sent {
                                last_sent = trimmed.clone();
                                let _ = on_text.send(trimmed);
                                if let Some(wfd) = wakeup_fd {
                                    let one: u64 = 1;
                                    unsafe {
                                        let _ = libc::write(
                                            wfd,
                                            &one as *const u64 as *const libc::c_void,
                                            8,
                                        );
                                    }
                                }
                            }
                        }
                    } else {
                        offer.destroy();
                    }
                }

                // 3. Deep-sleep Wayland + command eventfd (`0` idle wakeups!)
                let _ = conn.flush();
                if let Some(guard) = queue.prepare_read() {
                    let mut pfds = [
                        libc::pollfd {
                            fd: guard.connection_fd().as_raw_fd(),
                            events: libc::POLLIN,
                            revents: 0,
                        },
                        libc::pollfd {
                            fd: cmd_wake_fd,
                            events: libc::POLLIN,
                            revents: 0,
                        },
                    ];
                    let nfds = if cmd_wake_fd >= 0 { 2 } else { 1 };
                    let timeout = if cmd_wake_fd >= 0 { -1 } else { 250 };
                    let rc = unsafe { libc::poll(pfds.as_mut_ptr(), nfds as libc::nfds_t, timeout) };
                    if cmd_wake_fd >= 0 && (pfds[1].revents & libc::POLLIN) != 0 {
                        let mut sink = 0u64;
                        unsafe {
                            let _ = libc::read(
                                cmd_wake_fd,
                                &mut sink as *mut u64 as *mut libc::c_void,
                                8,
                            );
                        }
                    }
                    if rc > 0 && (pfds[0].revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR)) != 0 {
                        let _ = guard.read();
                    }
                } else {
                    std::thread::sleep(Duration::from_millis(20));
                }
                if queue.dispatch_pending(&mut state).is_err() {
                    break;
                }
            }
        })
        .ok();
    ClipboardCtl {
        tx: cmd_tx,
        cmd_wake_fd,
    }
}

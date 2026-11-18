// SPDX-License-Identifier: MIT
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Instant;

use tb520fu_ui::canvas::{Canvas, RectF};
use tb520fu_ui::clipboard::{spawn_native_clipboard, ClipboardCtl};
use tb520fu_ui::color::{Palette, Skin};
use tb520fu_ui::touch_gate::{GateVerdict, TouchGateShm};
use tb520fu_ui::wayland::{KbFocus, LayerConfig, LayerKind, SurfaceId, UiEvent, WaylandApp};

use tb520fu_osk::fcitx::{lookup_evdev_code, spawn_fcitx_bridge, FcitxEvent};
use tb520fu_osk::layouts::KbMode;
use tb520fu_osk::render::OskRenderer;
use tb520fu_osk::state::{OskState, OutgoingAction, PanelKind};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    // 1. Remote D-Bus CLI control (`tb520fu-osk show|hide|toggle|prime|reload_prefs`)
    if let Some(first) = args.first().map(|s| s.as_str()) {
        if matches!(first, "show" | "hide" | "toggle" | "prime" | "reload_prefs") {
            if let Err(e) = call_osk_dbus(first) {
                eprintln!("tb520fu-osk {first} failed: {e}");
                std::process::exit(1);
            }
            return;
        }
    }

    // 2. Headless Vulkan visual verification (`tb520fu-osk --render-png <path> ...`)
    if let Some(pos) = args.iter().position(|a| a == "--render-png") {
        let Some(out_path) = args.get(pos + 1) else {
            eprintln!("usage: tb520fu-osk --render-png <out.png> [--mode dock|split|float] [--panel ...] [--skin ...] [--composing]");
            std::process::exit(2);
        };
        tb520fu_ui::canvas::set_own_backdrop_default(true);
        if let Err(e) = run_headless_png(out_path, &args) {
            eprintln!("render-png failed: {e}");
            std::process::exit(1);
        }
        return;
    }

    // 3. Live Wayland Layer-Shell Daemon
    let mut preview_mode = false;
    let mut initial_mode: Option<KbMode> = None;
    for a in &args {
        if a == "--preview" {
            preview_mode = true;
        } else if let Some(m) = a.strip_prefix("--preview=") {
            preview_mode = true;
            initial_mode = match m {
                "split" => Some(KbMode::Split),
                "float" => Some(KbMode::Float),
                "dock" => Some(KbMode::Dock),
                _ => None,
            };
        }
    }

    if let Err(e) = run_wayland_daemon(preview_mode, initial_mode) {
        eprintln!("tb520fu-osk fatal: {e}");
        std::process::exit(1);
    }
}

fn call_osk_dbus(method: &str) -> Result<(), String> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    rt.block_on(async {
        let conn = zbus::Connection::session().await.map_err(|e| e.to_string())?;
        conn.call_method(
            Some("fyi.tb520fu.Osk"),
            "/Osk",
            Some("fyi.tb520fu.Osk"),
            method,
            &(),
        )
        .await
        .map_err(|e| e.to_string())?;
        Ok(())
    })
}

fn run_headless_png(out_path: &str, args: &[String]) -> Result<(), String> {
    let screen_w = 1472u32;
    let screen_h = 920u32;
    let scale = 2.0_f32;

    let mut state = OskState::new(screen_w, screen_h);
    let mut idx = 0;
    while idx < args.len() {
        match args[idx].as_str() {
            "--mode" if idx + 1 < args.len() => {
                state.prefs.mode = match args[idx + 1].as_str() {
                    "split" => KbMode::Split,
                    "float" => KbMode::Float,
                    _ => KbMode::Dock,
                };
                idx += 1;
            }
            "--skin" if idx + 1 < args.len() => {
                state.prefs.skin = Skin::from_name(&args[idx + 1]);
                state.pal = Palette::for_skin(state.prefs.skin);
                idx += 1;
            }
            "--panel" if idx + 1 < args.len() => {
                state.panel = match args[idx + 1].as_str() {
                    "toolbox" => PanelKind::Toolbox,
                    "feedback" => PanelKind::Feedback,
                    "edit" => PanelKind::TextEdit,
                    "sym" => PanelKind::Symbols,
                    "emoji" => PanelKind::Emoji,
                    "clip" => PanelKind::Clipboard,
                    "cands" => PanelKind::Candidates,
                    "layout" => PanelKind::LayoutPicker,
                    _ => PanelKind::None,
                };
                idx += 1;
            }
            "--num" => {
                state.layout = tb520fu_osk::layouts::KbLayout::Num;
            }
            "--composing" => {
                state.preedit = "ni'hao'shi'jie".into();
                state.candidates = vec![
                    "你好世界".into(),
                    "你好".into(),
                    "你号".into(),
                    "拟好".into(),
                    "逆行".into(),
                    "霓虹".into(),
                    "泥泞".into(),
                    "柠檬".into(),
                ];
            }
            _ => {}
        }
        idx += 1;
    }
    state.push_clipboard("https://github.com/YaLTeR/niri".into());
    state.push_clipboard("Lenovo Yoga Pad Pro 12.7 (TB520FU)".into());
    state.prefs.pinned.push("ssh debian@tablet.local".into());
    state.rebuild_geometry();
    state.shown = true;
    state.slide_y.snap(0.0);

    let phys_w = ((state.metrics.surf_w as f32) * scale).round() as u32;
    let phys_h = ((state.metrics.surf_h as f32) * scale).round() as u32;
    let surf_ox = if state.prefs.mode == KbMode::Float {
        state.prefs.float_x as f32
    } else {
        0.0
    };
    let surf_oy = if state.prefs.mode == KbMode::Float {
        state.prefs.float_y as f32
    } else {
        (screen_h as f32) - (state.metrics.surf_h as f32)
    };

    let mut canvas = Canvas::new(phys_w, phys_h, scale);
    canvas.set_screen_context(surf_ox, surf_oy, screen_w as f32, screen_h as f32);
    canvas.draw_wallpaper_background(
        RectF::new(
            0.0,
            0.0,
            state.metrics.surf_w as f32,
            state.metrics.surf_h as f32,
        ),
        1.0,
    );
    let mut renderer = OskRenderer::new();
    renderer.render_overlay(&mut state, &mut canvas);
    canvas.save_png(out_path)
}

pub fn layer_cfg_for(state: &OskState) -> LayerConfig {
    if state.prefs.mode == KbMode::Float {
        LayerConfig {
            layer: LayerKind::Overlay,
            anchor_top: true,
            anchor_bottom: false,
            anchor_left: true,
            anchor_right: false,
            width: state.metrics.surf_w,
            height: state.metrics.surf_h,
            exclusive_zone: -1,
            margin_top: state.prefs.float_y,
            margin_right: 0,
            margin_bottom: 0,
            margin_left: state.prefs.float_x,
            kb_focus: KbFocus::None,
        }
    } else {
        let excl = if state.shown && state.prefs.mode == KbMode::Dock {
            state.metrics.kb_h.round() as i32
        } else {
            0
        };
        LayerConfig {
            layer: LayerKind::Overlay,
            anchor_top: false,
            anchor_bottom: true,
            anchor_left: true,
            anchor_right: true,
            width: 0,
            height: state.metrics.surf_h,
            exclusive_zone: excl,
            margin_top: 0,
            margin_right: 0,
            margin_bottom: 0,
            margin_left: 0,
            kb_focus: KbFocus::None,
        }
    }
}

fn run_wayland_daemon(preview_mode: bool, initial_mode: Option<KbMode>) -> Result<(), String> {
    let mut wl = WaylandApp::connect()?;
    let touch_gate = TouchGateShm::open_or_create(None).ok();
    let mut pending_auto_show_until: Option<Instant> = None;

    let wakeup_fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
    let wakeup_opt = if wakeup_fd >= 0 { Some(wakeup_fd) } else { None };

    let kbd_state = tb520fu_osk::kbd_watch::KbdState::new();
    let (fcitx_tx, fcitx_rx) = spawn_fcitx_bridge(preview_mode, wakeup_opt, kbd_state.clone());
    if !preview_mode {
        tb520fu_osk::kbd_watch::spawn_kbd_watcher(kbd_state, fcitx_tx.clone());
    }
    let (clip_on_tx, clip_rx) = mpsc::channel::<String>();
    let clip_ctl = spawn_native_clipboard(clip_on_tx, wakeup_opt);

    let mut state = OskState::new(1472, 920);
    if let Some(m) = initial_mode {
        state.prefs.mode = m;
        state.rebuild_geometry();
    }
    if preview_mode {
        state.show(true);
    }

    let surf_id: SurfaceId = wl.create_layer_surface("tb520fu-osk", layer_cfg_for(&state), None)?;
    wl.set_keep_swapchain(surf_id, true);
    if !state.shown {
        wl.set_input_region(surf_id, Some(&[]));
    }

    let mut renderer = OskRenderer::new();
    let mut canvas = Canvas::new(64, 64, 2.0);
    {
        // Pre-warm glyph & icon atlases so first OSK pop-up takes 0ms!
        let pw = ((state.metrics.surf_w.max(1) as f32) * 2.0).round() as u32;
        let ph = ((state.metrics.surf_h.max(1) as f32) * 2.0).round() as u32;
        canvas.resize(pw, ph, 2.0);
        let was_shown = state.shown;
        state.shown = true;
        renderer.render(&mut state, &mut canvas);
        state.shown = was_shown;
        canvas.clear(tb520fu_ui::color::Rgba::TRANSPARENT);
        if let Some(vk_mtx) = tb520fu_ui::vk_renderer::VkRenderer::global() {
            let _ = vk_mtx.lock().unwrap().flush_pending_work();
        }
    }
    let mut prewarmed_swapchain = false;
    let mut surface_mapped = false;
    let mut last_tick = Instant::now();
    let mut needs_redraw = state.shown;
    let mut needs_frame = state.shown;

    loop {
        let wl_events = wl.poll_events()?;
        for ev in wl_events {
            match ev {
                UiEvent::Configured {
                    surface,
                    width,
                    height,
                    scale,
                } if surface == surf_id => {
                    if state.prefs.mode != KbMode::Float && width > 0 {
                        // Exact 16:10 logical screen height for any fractional/integer scale (e.g. 1.6 -> 1840x1150, portrait 1150x1840)
                        let est_h = if width < 1280 {
                            (width * 16 + 5) / 10
                        } else {
                            (width * 10 + 8) / 16
                        };
                        if state.screen_w != width {
                            state.set_screen_size(width, est_h);
                            wl.update_layer_config(surf_id, layer_cfg_for(&state));
                        }
                    }
                    let phys_w = ((width.max(1) as f32) * scale).round() as u32;
                    let phys_h = ((height.max(1) as f32) * scale).round() as u32;
                    canvas.resize(phys_w, phys_h, scale);
                    if !prewarmed_swapchain && phys_w > 64 && phys_h > 64 {
                        prewarmed_swapchain = true;
                        wl.prewarm_wsi_swapchain(surf_id, phys_w, phys_h);
                    }
                    if state.shown || state.slide_y.is_animating() {
                        needs_redraw = true;
                    } else {
                        wl.unmap_surface(surf_id);
                        wl.set_input_region(surf_id, Some(&[]));
                        surface_mapped = false;
                    }
                }
                UiEvent::Frame { surface, .. } if surface == surf_id => {
                    let now = Instant::now();
                    let dt = now.duration_since(last_tick).as_secs_f32().min(0.05);
                    last_tick = now;
                    let (rd, nf) = state.tick(dt);
                    needs_redraw |= rd;
                    needs_frame = nf;
                }
                UiEvent::TouchDown {
                    surface,
                    slot,
                    x,
                    y,
                    ..
                } if surface == surf_id => {
                    if state.on_touch_down(slot, x, y) {
                        needs_redraw = true;
                        needs_frame = true;
                    }
                }
                UiEvent::TouchMotion {
                    surface,
                    slot,
                    x,
                    y,
                    ..
                } if surface == surf_id => {
                    if state.on_touch_motion(slot, x, y) {
                        needs_redraw = true;
                    }
                }
                UiEvent::TouchUp { surface, slot, .. } if surface == surf_id => {
                    if state.on_touch_up(slot) {
                        needs_redraw = true;
                    }
                }
                UiEvent::TouchCancel { surface } if surface == surf_id => {
                    if state.on_touch_cancel() {
                        needs_redraw = true;
                    }
                }
                UiEvent::Closed { surface } if surface == surf_id => return Ok(()),
                _ => {}
            }
        }

        // 2. Drain Fcitx5 & D-Bus events
        while let Ok(fev) = fcitx_rx.try_recv() {
            match fev {
                FcitxEvent::ShowRequested { forced } => {
                    if !forced && !state.prefs.auto_popup {
                        pending_auto_show_until = None;
                        continue;
                    }
                    if forced {
                        boost_gpu_async(550);
                        pending_auto_show_until = None;
                        state.show(true);
                        wl.update_layer_config(surf_id, layer_cfg_for(&state));
                        needs_redraw = true;
                        needs_frame = true;
                    } else if let Some(gate) = &touch_gate {
                        match gate.snapshot().allow_auto_show() {
                            GateVerdict::Allow(_) => {
                                boost_gpu_async(550);
                                pending_auto_show_until = None;
                                state.show(false);
                                wl.update_layer_config(surf_id, layer_cfg_for(&state));
                                needs_redraw = true;
                                needs_frame = true;
                            }
                            GateVerdict::WaitFingerUp => {
                                pending_auto_show_until =
                                    Some(Instant::now() + std::time::Duration::from_millis(650));
                            }
                            GateVerdict::Suppress(_) => {
                                pending_auto_show_until = None;
                            }
                        }
                    } else {
                        boost_gpu_async(550);
                        state.show(false);
                        wl.update_layer_config(surf_id, layer_cfg_for(&state));
                        needs_redraw = true;
                        needs_frame = true;
                    }
                }
                FcitxEvent::HideRequested { forced } => {
                    pending_auto_show_until = None;
                    if (forced || !state.forced_open) && state.shown {
                        boost_gpu_async(450);
                        state.hide(false);
                        wl.update_layer_config(surf_id, layer_cfg_for(&state));
                        needs_redraw = true;
                        needs_frame = true;
                    } else if forced && !state.shown {
                        wl.update_layer_config(surf_id, layer_cfg_for(&state));
                        if surface_mapped {
                            wl.unmap_surface(surf_id);
                            wl.set_input_region(surf_id, Some(&[]));
                            surface_mapped = false;
                        }
                    }
                }
                FcitxEvent::ToggleRequested => {
                    boost_gpu_async(550);
                    if state.shown {
                        state.hide(true);
                    } else {
                        state.show(true);
                    }
                    wl.update_layer_config(surf_id, layer_cfg_for(&state));
                    needs_redraw = true;
                    needs_frame = true;
                }
                FcitxEvent::PreeditChanged(p) => {
                    state.fcitx_online = true;
                    if state.preedit != p {
                        state.preedit = p;
                        if state.preedit.is_empty()
                            && state.candidates.is_empty()
                            && state.panel == PanelKind::Candidates
                        {
                            state.panel = PanelKind::None;
                        }
                        if state.shown {
                            needs_redraw = true;
                        }
                    }
                }
                FcitxEvent::CandidatesChanged {
                    list,
                    has_prev,
                    has_next,
                    cursor,
                } => {
                    state.fcitx_online = true;
                    state.candidates = list;
                    state.has_prev = has_prev;
                    state.has_next = has_next;
                    state.cand_cursor = cursor;
                    state.cand_scroll_x = 0.0;
                    if state.candidates.is_empty() && state.panel == PanelKind::Candidates {
                        state.panel = PanelKind::None;
                    }
                    if state.shown {
                        needs_redraw = true;
                    }
                }
                FcitxEvent::LangChanged(lang) => {
                    state.fcitx_online = true;
                    if state.lang != lang {
                        state.lang = lang;
                        state.rebuild_geometry();
                        if state.shown {
                            needs_redraw = true;
                        }
                    }
                }
                FcitxEvent::FallbackPasteCommit(text) => {
                    paste_via_clipboard_and_vk(&clip_ctl, &mut wl, &text);
                }
                FcitxEvent::PrimeRequested => {
                    let _ = fcitx_tx.send(tb520fu_osk::fcitx::FcitxCmd::PrimeOnScreenMode);
                }
                FcitxEvent::ReloadPrefs => {
                    state.reload_prefs_from_disk();
                    wl.update_layer_config(surf_id, layer_cfg_for(&state));
                    if state.shown {
                        needs_redraw = true;
                        needs_frame = true;
                    }
                }
            }
        }

        // 3. Drain event-driven `zwlr-data-control-v1` clipboard watcher
        while let Ok(item) = clip_rx.try_recv() {
            state.push_clipboard(item);
            if state.shown && state.panel == PanelKind::Clipboard {
                needs_redraw = true;
            }
        }

        // 3b. Re-check deferred `WaitFingerUp` auto-show once finger lifts
        if let Some(deadline) = pending_auto_show_until {
            if Instant::now() >= deadline {
                pending_auto_show_until = None;
            } else if let Some(gate) = &touch_gate {
                match gate.snapshot().allow_auto_show() {
                    GateVerdict::Allow(_) => {
                        pending_auto_show_until = None;
                        state.show(false);
                        wl.update_layer_config(surf_id, layer_cfg_for(&state));
                        needs_redraw = true;
                        needs_frame = true;
                    }
                    GateVerdict::Suppress(_) => {
                        pending_auto_show_until = None;
                    }
                    GateVerdict::WaitFingerUp => {}
                }
            }
        }

        // 4. Timer tick
        let now = Instant::now();
        let elapsed = now.duration_since(last_tick).as_secs_f32();
        if elapsed >= 0.0069 {
            last_tick = now;
            let (rd, nf) = state.tick(elapsed.min(0.05));
            needs_redraw |= rd;
            needs_frame |= nf;
        }

        // 5. Outgoing actions
        let actions: Vec<OutgoingAction> = state.outbox.drain(..).collect();
        for act in actions {
            match act {
                OutgoingAction::Fcitx(cmd) => {
                    let _ = fcitx_tx.send(cmd);
                }
                OutgoingAction::DirectText(text) => {
                    paste_via_clipboard_and_vk(&clip_ctl, &mut wl, &text);
                }
                OutgoingAction::DirectBackspace => {
                    if let Some(ev) = lookup_evdev_code("BackSpace") {
                        wl.send_vk_combo(ev, 0);
                    }
                }
                OutgoingAction::DirectEnter => {
                    if let Some(ev) = lookup_evdev_code("Return") {
                        wl.send_vk_combo(ev, 0);
                    }
                }
                OutgoingAction::DirectClearAll => {
                    if let (Some(a_ev), Some(bs_ev)) =
                        (lookup_evdev_code("a"), lookup_evdev_code("BackSpace"))
                    {
                        wl.send_vk_combo(a_ev, 4);
                        wl.send_vk_combo(bs_ev, 0);
                    }
                }
                OutgoingAction::VkCombo { evdev_code, mods } => {
                    wl.send_vk_combo(evdev_code, mods);
                }
                OutgoingAction::ReconfigureSurface => {
                    wl.update_layer_config(surf_id, layer_cfg_for(&state));
                    needs_redraw = true;
                    if state.shown || state.slide_y.is_animating() {
                        needs_frame = true;
                    }
                }
                OutgoingAction::UnmapSurface => {
                    wl.update_layer_config(surf_id, layer_cfg_for(&state));
                    wl.unmap_surface(surf_id);
                    wl.set_input_region(surf_id, Some(&[]));
                    surface_mapped = false;
                    needs_redraw = false;
                    needs_frame = false;
                }
            }
        }

        // 6. Render & present when dirty
        if needs_redraw && (state.shown || state.slide_y.is_animating()) && wl.frame_ready(surf_id) {
            if let Some((lw, lh, sc)) = wl.surface_geometry(surf_id) {
                let pw = ((lw as f32) * sc).round() as u32;
                let ph = ((lh as f32) * sc).round() as u32;
                canvas.resize(pw, ph, sc);
                let sox = if state.prefs.mode == KbMode::Float {
                    state.prefs.float_x as f32
                } else {
                    0.0
                };
                let soy = if state.prefs.mode == KbMode::Float {
                    state.prefs.float_y as f32
                } else {
                    (state.screen_h as f32) - (lh as f32)
                };
                canvas.set_screen_context(sox, soy, state.screen_w as f32, state.screen_h as f32);
                renderer.render(&mut state, &mut canvas);
                let regions = state.metrics.input_regions(
                    state.prefs.mode,
                    state.split_glass_active(),
                    state.current_slide_offset(),
                );
                wl.set_input_region(surf_id, Some(&regions));
                if wl.present(surf_id, &canvas, needs_frame) {
                    surface_mapped = true;
                    needs_redraw = false;
                }
            }
        }

        if !state.shown && !state.slide_y.is_animating() && surface_mapped {
            wl.update_layer_config(surf_id, layer_cfg_for(&state));
            wl.unmap_surface(surf_id);
            wl.set_input_region(surf_id, Some(&[]));
            surface_mapped = false;
            needs_redraw = false;
            needs_frame = false;
        }

        // 7. Deep sleep in `poll()` when idle
        let timeout_ms = if pending_auto_show_until.is_some() {
            12
        } else if !surface_mapped && !state.shown {
            -1
        } else if needs_frame || needs_redraw {
            7
        } else {
            250
        };

        let mut pfds = [
            libc::pollfd {
                fd: wl.raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: wakeup_fd,
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        let nfds = if wakeup_fd >= 0 { 2 } else { 1 };
        unsafe {
            let timeout_ms = if wl.has_queued_events() { 0 } else { timeout_ms };
            let _ = libc::poll(pfds.as_mut_ptr(), nfds as libc::nfds_t, timeout_ms);
            if wakeup_fd >= 0 && (pfds[1].revents & libc::POLLIN) != 0 {
                let mut sink = 0u64;
                let _ = libc::read(wakeup_fd, &mut sink as *mut u64 as *mut libc::c_void, 8);
            }
        }
    }
}

fn paste_via_clipboard_and_vk(
    clip_ctl: &ClipboardCtl,
    wl: &mut WaylandApp,
    text: &str,
) {
    if !clip_ctl.set_text_ack(text.to_string()) {
        // Fallback to detached `wl-copy` only if compositor lacks `zwlr-data-control-v1`
        let owned = text.to_string();
        std::thread::spawn(move || {
            let _ = Command::new("wl-copy")
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .and_then(|mut child| {
                    if let Some(mut stdin) = child.stdin.take() {
                        let _ = stdin.write_all(owned.as_bytes());
                    }
                    child.wait()
                });
        });
    }
    if let Some(v_ev) = lookup_evdev_code("v") {
        wl.send_vk_combo(v_ev, 4); // Ctrl+V
    }
}

fn boost_gpu_async(duration_ms: u16) {
    std::thread::spawn(move || {
        if let Ok(mut s) = std::os::unix::net::UnixStream::connect("/run/tb520fu-daemon.sock") {
            let _ = s.set_write_timeout(Some(std::time::Duration::from_millis(25)));
            let line = format!("{{\"cmd\":\"gpu_boost\",\"duration_ms\":{duration_ms}}}\n");
            let _ = s.write_all(line.as_bytes());
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hide_emits_reconfigure_and_clears_dock_exclusive_zone_immediately() {
        let mut st = OskState::new(1472, 920);
        st.prefs.mode = KbMode::Dock;
        st.rebuild_geometry();
        assert_eq!(layer_cfg_for(&st).exclusive_zone, 0);

        st.show(true);
        assert!(st.shown);
        assert!(st.outbox.iter().any(|a| matches!(a, OutgoingAction::ReconfigureSurface)));
        assert!(layer_cfg_for(&st).exclusive_zone > 300);
        st.outbox.clear();

        // Simulate tapping the OSK collapse button (`state.hide(true)`)
        st.hide(true);
        assert!(!st.shown);
        assert!(
            st.outbox.iter().any(|a| matches!(a, OutgoingAction::ReconfigureSurface)),
            "hide() must emit ReconfigureSurface immediately so exclusive_zone drops to 0 while mapped"
        );
        assert_eq!(layer_cfg_for(&st).exclusive_zone, 0);
    }
}

// SPDX-License-Identifier: MIT
use std::collections::HashMap;
use std::ffi::CString;
use std::os::fd::{AsFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};

use wayland_client::protocol::{
    wl_buffer, wl_callback, wl_compositor, wl_keyboard, wl_output, wl_pointer, wl_region,
    wl_registry, wl_seat, wl_shm, wl_shm_pool, wl_surface, wl_touch,
};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum};
use wayland_protocols::ext::session_lock::v1::client::{
    ext_session_lock_manager_v1, ext_session_lock_surface_v1, ext_session_lock_v1,
};
use wayland_protocols::wp::fractional_scale::v1::client::{
    wp_fractional_scale_manager_v1, wp_fractional_scale_v1,
};
use wayland_protocols::ext::background_effect::v1::client::{
    ext_background_effect_manager_v1, ext_background_effect_surface_v1,
};
use wayland_protocols::wp::cursor_shape::v1::client::{
    wp_cursor_shape_device_v1, wp_cursor_shape_manager_v1,
};
use wayland_protocols::wp::viewporter::client::{wp_viewport, wp_viewporter};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1, zwp_virtual_keyboard_v1,
};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1, zwlr_layer_surface_v1,
};

use crate::canvas::{Canvas, RectF};
use crate::vk_renderer::VkRenderer;

pub type SurfaceId = u32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerKind {
    Background,
    Bottom,
    Top,
    Overlay,
}

impl LayerKind {
    fn to_wlr(self) -> zwlr_layer_shell_v1::Layer {
        match self {
            Self::Background => zwlr_layer_shell_v1::Layer::Background,
            Self::Bottom => zwlr_layer_shell_v1::Layer::Bottom,
            Self::Top => zwlr_layer_shell_v1::Layer::Top,
            Self::Overlay => zwlr_layer_shell_v1::Layer::Overlay,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KbFocus {
    None,
    Exclusive,
    OnDemand,
}

impl KbFocus {
    fn to_wlr(self) -> zwlr_layer_surface_v1::KeyboardInteractivity {
        match self {
            Self::None => zwlr_layer_surface_v1::KeyboardInteractivity::None,
            Self::Exclusive => zwlr_layer_surface_v1::KeyboardInteractivity::Exclusive,
            Self::OnDemand => zwlr_layer_surface_v1::KeyboardInteractivity::OnDemand,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayerConfig {
    pub layer: LayerKind,
    pub anchor_top: bool,
    pub anchor_bottom: bool,
    pub anchor_left: bool,
    pub anchor_right: bool,
    pub width: u32,
    pub height: u32,
    pub exclusive_zone: i32,
    pub margin_top: i32,
    pub margin_right: i32,
    pub margin_bottom: i32,
    pub margin_left: i32,
    pub kb_focus: KbFocus,
}

impl LayerConfig {
    pub fn anchor_bits(&self) -> zwlr_layer_surface_v1::Anchor {
        let mut a = zwlr_layer_surface_v1::Anchor::empty();
        if self.anchor_top {
            a |= zwlr_layer_surface_v1::Anchor::Top;
        }
        if self.anchor_bottom {
            a |= zwlr_layer_surface_v1::Anchor::Bottom;
        }
        if self.anchor_left {
            a |= zwlr_layer_surface_v1::Anchor::Left;
        }
        if self.anchor_right {
            a |= zwlr_layer_surface_v1::Anchor::Right;
        }
        a
    }
}

/// Unified input & lifecycle events delivered per `SurfaceId`.
#[derive(Debug, Clone)]
pub enum UiEvent {
    Configured {
        surface: SurfaceId,
        width: u32,
        height: u32,
        scale: f32,
    },
    Closed {
        surface: SurfaceId,
    },
    Frame {
        surface: SurfaceId,
        time_ms: u32,
    },
    /// Multi-touch or pointer press (`slot = -1` for mouse/stylus pointer).
    TouchDown {
        surface: SurfaceId,
        slot: i32,
        x: f32,
        y: f32,
        time_ms: u32,
    },
    TouchMotion {
        surface: SurfaceId,
        slot: i32,
        x: f32,
        y: f32,
        time_ms: u32,
    },
    TouchUp {
        surface: SurfaceId,
        slot: i32,
        time_ms: u32,
    },
    TouchCancel {
        surface: SurfaceId,
    },
    Scroll {
        surface: SurfaceId,
        x: f32,
        y: f32,
        dx: f32,
        dy: f32,
    },
    ScrollStop {
        surface: SurfaceId,
    },
    PointerActivity {
        surface: SurfaceId,
    },
    PointerSecondaryClick {
        surface: SurfaceId,
        x: f32,
        y: f32,
    },
    KeyPress {
        evdev_code: u32,
        pressed: bool,
    },
    SessionLocked,
    SessionLockFinished,
}

struct ShmSlot {
    fd: OwnedFd,
    ptr: *mut u8,
    cap: usize,
    pool: wl_shm_pool::WlShmPool,
    buffer: Option<wl_buffer::WlBuffer>,
    buf_w: u32,
    buf_h: u32,
    busy: bool,
}

unsafe impl Send for ShmSlot {}

impl ShmSlot {
    fn new(shm: &wl_shm::WlShm, qh: &QueueHandle<WlState>, id: SurfaceId, slot_idx: usize, cap: usize) -> std::io::Result<Self> {
        let name = CString::new("tb520fu-shm").unwrap();
        let raw = unsafe { libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC) };
        if raw < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        if unsafe { libc::ftruncate(raw, cap as libc::off_t) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                cap,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                raw,
                0,
            )
        };
        if ptr == libc::MAP_FAILED {
            return Err(std::io::Error::last_os_error());
        }
        let pool = shm.create_pool(fd.as_fd(), cap as i32, qh, ());
        let _ = (id, slot_idx);
        Ok(Self {
            fd,
            ptr: ptr as *mut u8,
            cap,
            pool,
            buffer: None,
            buf_w: 0,
            buf_h: 0,
            busy: false,
        })
    }

    fn ensure_buffer(
        &mut self,
        qh: &QueueHandle<WlState>,
        surf_id: SurfaceId,
        slot_idx: usize,
        phys_w: u32,
        phys_h: u32,
    ) -> std::io::Result<&wl_buffer::WlBuffer> {
        let needed = (phys_w as usize) * (phys_h as usize) * 4;
        if needed > self.cap {
            let new_cap = needed.next_power_of_two().max(needed);
            if unsafe { libc::ftruncate(self.fd.as_fd().as_raw_fd(), new_cap as libc::off_t) } < 0 {
                return Err(std::io::Error::last_os_error());
            }
            let new_ptr = unsafe {
                libc::mremap(self.ptr as *mut libc::c_void, self.cap, new_cap, libc::MREMAP_MAYMOVE)
            };
            if new_ptr == libc::MAP_FAILED {
                return Err(std::io::Error::last_os_error());
            }
            self.ptr = new_ptr as *mut u8;
            self.cap = new_cap;
            self.pool.resize(new_cap as i32);
        }
        if self.buf_w != phys_w || self.buf_h != phys_h || self.buffer.is_none() {
            if let Some(old) = self.buffer.take() {
                old.destroy();
            }
            let b = self.pool.create_buffer(
                0,
                phys_w as i32,
                phys_h as i32,
                (phys_w * 4) as i32,
                wl_shm::Format::Argb8888,
                qh,
                (surf_id, slot_idx),
            );
            self.buf_w = phys_w;
            self.buf_h = phys_h;
            self.buffer = Some(b);
        }
        Ok(self.buffer.as_ref().unwrap())
    }

    fn slice_mut(&mut self, len: usize) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.ptr, len.min(self.cap)) }
    }
}

impl Drop for ShmSlot {
    fn drop(&mut self) {
        if let Some(b) = self.buffer.take() {
            b.destroy();
        }
        self.pool.destroy();
        unsafe {
            libc::munmap(self.ptr as *mut libc::c_void, self.cap);
        }
    }
}

use std::os::fd::AsRawFd;

enum RoleObj {
    Layer(zwlr_layer_surface_v1::ZwlrLayerSurfaceV1),
    Lock(ext_session_lock_surface_v1::ExtSessionLockSurfaceV1),
}

struct ManagedSurface {
    wl_surface: wl_surface::WlSurface,
    role: RoleObj,
    layer_cfg: Option<LayerConfig>,
    viewport: Option<wp_viewport::WpViewport>,
    frac_scale: Option<wp_fractional_scale_v1::WpFractionalScaleV1>,
    /// ext-background-effect object, created the first time the surface draws glass.
    bg_effect: Option<ext_background_effect_surface_v1::ExtBackgroundEffectSurfaceV1>,
    /// Hash of the blur region last sent, so an unchanged region is not re-sent every frame.
    blur_sig: u64,
    /// Hash of the input region last sent, avoiding redundant `wl_surface.commit()` every frame.
    input_sig: u64,
    logical_w: u32,
    logical_h: u32,
    scale_120: u32,
    configured: bool,
    mapped: bool,
    frame_pending: bool,
    /// When the frame callback that `frame_pending` is waiting for was requested.
    frame_sent_at: Option<std::time::Instant>,
    /// Set when the surface was unmapped with its swapchain kept; see `KEEP_SWAPCHAIN`.
    unmapped_at: Option<std::time::Instant>,
    keep_swapchain: bool,
    wsi_failed: bool,
    slots: [ShmSlot; 2],
}

/// How long an unmapped surface keeps its Vulkan swapchain. Creating one for a full-screen surface
/// takes 50-90 ms (the kernel has to allocate and clear ~90 MB), which showed up as a hitch on every
/// open of the pull-down console. Keeping it for a while makes re-opening instant; after this long
/// unused the memory is given back.
const KEEP_SWAPCHAIN: std::time::Duration = std::time::Duration::from_secs(45);

#[derive(Clone)]
pub struct OutputInfo {
    pub global_name: u32,
    pub output: wl_output::WlOutput,
    pub name: String,
}

pub struct WlState {
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    layer_shell: Option<zwlr_layer_shell_v1::ZwlrLayerShellV1>,
    viewporter: Option<wp_viewporter::WpViewporter>,
    frac_mgr: Option<wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1>,
    bg_effect_mgr: Option<ext_background_effect_manager_v1::ExtBackgroundEffectManagerV1>,
    lock_mgr: Option<ext_session_lock_manager_v1::ExtSessionLockManagerV1>,
    vk_mgr: Option<zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1>,
    cursor_shape_mgr: Option<wp_cursor_shape_manager_v1::WpCursorShapeManagerV1>,
    cursor_shape_dev: Option<wp_cursor_shape_device_v1::WpCursorShapeDeviceV1>,
    seat: Option<wl_seat::WlSeat>,
    pointer: Option<wl_pointer::WlPointer>,
    touch: Option<wl_touch::WlTouch>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    vk: Option<zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1>,
    vk_mods: u32,
    session_lock: Option<ext_session_lock_v1::ExtSessionLockV1>,
    outputs: Vec<OutputInfo>,

    surfaces: HashMap<SurfaceId, ManagedSurface>,
    wl_id_to_surf: HashMap<u32, SurfaceId>,
    touch_focus: HashMap<i32, SurfaceId>,
    pointer_focus: Option<SurfaceId>,
    pointer_pos: (f32, f32),
    pointer_down: bool,
    last_pointer_activity: Option<std::time::Instant>,
    next_id: SurfaceId,
    pub events: Vec<UiEvent>,
}

pub struct WaylandApp {
    pub conn: Connection,
    pub queue: EventQueue<WlState>,
    pub qh: QueueHandle<WlState>,
    pub state: WlState,
}

impl WaylandApp {
    pub fn connect() -> Result<Self, String> {
        let conn = Connection::connect_to_env().map_err(|e| format!("Wayland connect failed: {e}"))?;
        let mut queue = conn.new_event_queue();
        let qh = queue.handle();
        let mut state = WlState {
            compositor: None,
            shm: None,
            layer_shell: None,
            viewporter: None,
            frac_mgr: None,
            bg_effect_mgr: None,
            lock_mgr: None,
            vk_mgr: None,
            cursor_shape_mgr: None,
            cursor_shape_dev: None,
            seat: None,
            pointer: None,
            touch: None,
            keyboard: None,
            vk: None,
            vk_mods: 0,
            session_lock: None,
            outputs: Vec::new(),
            surfaces: HashMap::new(),
            wl_id_to_surf: HashMap::new(),
            touch_focus: HashMap::new(),
            pointer_focus: None,
            pointer_pos: (0.0, 0.0),
            pointer_down: false,
            last_pointer_activity: None,
            next_id: 1,
            events: Vec::new(),
        };
        let _registry = conn.display().get_registry(&qh, ());
        queue
            .roundtrip(&mut state)
            .map_err(|e| format!("Wayland initial roundtrip failed: {e}"))?;
        queue
            .roundtrip(&mut state)
            .map_err(|e| format!("Wayland second roundtrip failed: {e}"))?;
        Ok(Self {
            conn,
            queue,
            qh,
            state,
        })
    }

    pub fn raw_fd(&self) -> RawFd {
        self.conn.backend().poll_fd().as_raw_fd()
    }

    /// True when events for this queue were already read off the socket and are waiting to be
    /// dispatched. The Vulkan driver reads the same socket for its own queue while presenting, so
    /// our events (frame callbacks, touch) can be sitting here with nothing left on the fd for
    /// `poll()` to wake on. Callers must not sleep while this returns true.
    pub fn has_queued_events(&mut self) -> bool {
        match self.queue.prepare_read() {
            Some(guard) => {
                drop(guard);
                false
            }
            None => true,
        }
    }

    fn release_idle_swapchains(&mut self) {
        let mut idle = Vec::new();
        for (id, ms) in self.state.surfaces.iter_mut() {
            if !ms.keep_swapchain
                && !ms.mapped
                && ms.unmapped_at.map(|t| t.elapsed() > KEEP_SWAPCHAIN).unwrap_or(false)
            {
                ms.unmapped_at = None;
                idle.push(*id);
            }
        }
        if !idle.is_empty() {
            if let Some(vk_mtx) = VkRenderer::global() {
                let mut vk = vk_mtx.lock().unwrap();
                for id in idle {
                    vk.destroy_wsi_surface(id);
                }
            }
        }
    }

    /// Non-blocking read + dispatch of pending Wayland events into `self.state.events`.
    pub fn poll_events(&mut self) -> Result<Vec<UiEvent>, String> {
        self.release_idle_swapchains();
        let _ = self.conn.flush();
        if let Some(guard) = self.queue.prepare_read() {
            // Poll fd with 0 timeout before reading
            let mut pfd = libc::pollfd {
                fd: guard.connection_fd().as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let rc = unsafe { libc::poll(&mut pfd, 1, 0) };
            if rc > 0 && (pfd.revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR)) != 0 {
                let _ = guard.read();
            }
        }
        self.queue
            .dispatch_pending(&mut self.state)
            .map_err(|e| format!("Wayland dispatch failed: {e}"))?;
        let _ = self.conn.flush();
        Ok(std::mem::take(&mut self.state.events))
    }

    pub fn flush(&self) {
        let _ = self.conn.flush();
    }

    pub fn outputs(&self) -> &[OutputInfo] {
        &self.state.outputs
    }

    /// Create a new `zwlr_layer_shell_v1` surface (e.g. top bar, control center, OSK, OSD).
    pub fn create_layer_surface(
        &mut self,
        namespace: &str,
        cfg: LayerConfig,
        output: Option<&wl_output::WlOutput>,
    ) -> Result<SurfaceId, String> {
        let comp = self
            .state
            .compositor
            .as_ref()
            .ok_or("missing wl_compositor")?;
        let shm = self.state.shm.as_ref().ok_or("missing wl_shm")?;
        let ls = self
            .state
            .layer_shell
            .as_ref()
            .ok_or("missing zwlr_layer_shell_v1")?;

        let id = self.state.next_id;
        self.state.next_id += 1;

        let wl_surface = comp.create_surface(&self.qh, id);
        let wl_obj_id = wl_surface.id().protocol_id();
        self.state.wl_id_to_surf.insert(wl_obj_id, id);

        let layer_surf = ls.get_layer_surface(
            &wl_surface,
            output,
            cfg.layer.to_wlr(),
            namespace.to_string(),
            &self.qh,
            id,
        );
        layer_surf.set_anchor(cfg.anchor_bits());
        layer_surf.set_size(cfg.width, cfg.height);
        layer_surf.set_exclusive_zone(cfg.exclusive_zone);
        layer_surf.set_margin(cfg.margin_top, cfg.margin_right, cfg.margin_bottom, cfg.margin_left);
        layer_surf.set_keyboard_interactivity(cfg.kb_focus.to_wlr());

        let viewport = self
            .state
            .viewporter
            .as_ref()
            .map(|vp| vp.get_viewport(&wl_surface, &self.qh, ()));
        let frac_scale = self
            .state
            .frac_mgr
            .as_ref()
            .map(|fm| fm.get_fractional_scale(&wl_surface, &self.qh, id));

        wl_surface.commit();

        let init_cap = ((cfg.width.max(64) * cfg.height.max(64)) as usize * 4).max(65536);
        let s0 = ShmSlot::new(shm, &self.qh, id, 0, init_cap).map_err(|e| e.to_string())?;
        let s1 = ShmSlot::new(shm, &self.qh, id, 1, init_cap).map_err(|e| e.to_string())?;

        self.state.surfaces.insert(
            id,
            ManagedSurface {
                wl_surface,
                role: RoleObj::Layer(layer_surf),
                layer_cfg: Some(cfg),
                viewport,
                frac_scale,
                logical_w: cfg.width,
                logical_h: cfg.height,
                scale_120: 120,
                configured: false,
                mapped: false,
                bg_effect: None,
                blur_sig: 0,
                input_sig: 0,
                frame_pending: false,
                frame_sent_at: None,
                unmapped_at: None,
                keep_swapchain: false,
                wsi_failed: false,
                slots: [s0, s1],
            },
        );
        let _ = self.conn.flush();
        Ok(id)
    }

    pub fn update_layer_config(&mut self, id: SurfaceId, cfg: LayerConfig) {
        if let Some(ms) = self.state.surfaces.get_mut(&id) {
            if ms.layer_cfg == Some(cfg) {
                return;
            }
            ms.layer_cfg = Some(cfg);
            if let RoleObj::Layer(ls) = &ms.role {
                ls.set_layer(cfg.layer.to_wlr());
                ls.set_anchor(cfg.anchor_bits());
                ls.set_size(cfg.width, cfg.height);
                ls.set_exclusive_zone(cfg.exclusive_zone);
                ls.set_margin(cfg.margin_top, cfg.margin_right, cfg.margin_bottom, cfg.margin_left);
                ls.set_keyboard_interactivity(cfg.kb_focus.to_wlr());
                ms.wl_surface.commit();
                let _ = self.conn.flush();
            }
        }
    }

    pub fn destroy_surface(&mut self, id: SurfaceId) {
        if let Some(vk_mtx) = VkRenderer::global() {
            vk_mtx.lock().unwrap().destroy_wsi_surface(id);
        }
        if let Some(ms) = self.state.surfaces.remove(&id) {
            self.state.wl_id_to_surf.remove(&ms.wl_surface.id().protocol_id());
            if let Some(fs) = ms.frac_scale {
                fs.destroy();
            }
            if let Some(vp) = ms.viewport {
                vp.destroy();
            }
            match ms.role {
                RoleObj::Layer(ls) => ls.destroy(),
                RoleObj::Lock(ls) => ls.destroy(),
            }
            ms.wl_surface.destroy();
            let _ = self.conn.flush();
        }
    }

    /// Update pixel-exact click-through mask: touches outside `rects` pass straight through to windows underneath!
    /// Pass `None` to accept input over the entire surface, or `Some(&[])` for 100% click-through.
    pub fn set_input_region(&mut self, id: SurfaceId, rects: Option<&[RectF]>) {
        let (Some(comp), Some(ms)) = (&self.state.compositor, self.state.surfaces.get_mut(&id)) else {
            return;
        };
        let sig = match rects {
            None => 1u64,
            Some([]) => 2u64,
            Some(list) => {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                for r in list {
                    (r.x.floor() as i32).hash(&mut h);
                    (r.y.floor() as i32).hash(&mut h);
                    (r.w.ceil() as i32).hash(&mut h);
                    (r.h.ceil() as i32).hash(&mut h);
                }
                h.finish().max(3)
            }
        };
        if ms.input_sig == sig {
            return;
        }
        ms.input_sig = sig;
        match rects {
            None => ms.wl_surface.set_input_region(None),
            Some(list) => {
                let region = comp.create_region(&self.qh, ());
                for r in list {
                    if r.w > 0.0 && r.h > 0.0 {
                        region.add(
                            r.x.floor() as i32,
                            r.y.floor() as i32,
                            r.w.ceil() as i32,
                            r.h.ceil() as i32,
                        );
                    }
                }
                ms.wl_surface.set_input_region(Some(&region));
                region.destroy();
            }
        }
        if ms.mapped {
            ms.wl_surface.commit();
            let _ = self.conn.flush();
        }
    }

    pub fn surface_geometry(&self, id: SurfaceId) -> Option<(u32, u32, f32)> {
        let ms = self.state.surfaces.get(&id)?;
        if !ms.configured || ms.logical_w == 0 || ms.logical_h == 0 {
            return None;
        }
        Some((ms.logical_w, ms.logical_h, (ms.scale_120 as f32) / 120.0))
    }

    /// Opt a static surface (such as `tb520fu-wallpaper`) out of Vulkan WSI swapchains so it uses a
    /// single lightweight `wl_shm` + `wp_viewport` buffer instead of pinning ~65 MB of swapchain VRAM.
    pub fn set_prefer_shm(&mut self, id: SurfaceId, prefer_shm: bool) {
        if let Some(ms) = self.state.surfaces.get_mut(&id) {
            ms.wsi_failed = prefer_shm;
        }
    }

    /// Pin an interactive layer surface's Vulkan WSI swapchain (`curtain`, `osk`) in VRAM across unmaps
    /// so pulling down after >45s idle never stalls on swapchain recreation.
    pub fn set_keep_swapchain(&mut self, id: SurfaceId, keep: bool) {
        if let Some(ms) = self.state.surfaces.get_mut(&id) {
            ms.keep_swapchain = keep;
        }
    }

    /// Pre-create the Vulkan WSI swapchain for `id` before it is mapped so the first touch frame takes `0.0 ms`.
    pub fn prewarm_wsi_swapchain(&mut self, id: SurfaceId, phys_w: u32, phys_h: u32) {
        let Some(ms) = self.state.surfaces.get(&id) else {
            return;
        };
        if ms.wsi_failed {
            return;
        }
        if let Some(vk_mtx) = VkRenderer::global() {
            let wl_display_ptr = self.conn.backend().display_ptr() as *mut std::ffi::c_void;
            let wl_surface_ptr = ms.wl_surface.id().as_ptr() as *mut std::ffi::c_void;
            let mut vk = vk_mtx.lock().unwrap();
            let _ = vk.prewarm_wsi_swapchain(
                id,
                wl_display_ptr,
                wl_surface_ptr,
                phys_w.max(1),
                phys_h.max(1),
            );
        }
    }

    /// Present `Canvas` to `id` via zero-copy Vulkan Wayland WSI (`VK_KHR_wayland_surface` + `FIFO` VSync)
    /// on Turnip/hardware GPUs, with automatic fallback to Vulkan offscreen -> double-buffered `wl_shm` + `wp_viewport`!
    pub fn present(&mut self, id: SurfaceId, canvas: &Canvas, request_next_frame: bool) -> bool {
        let t0 = crate::perf::start();
        let ok = self.present_inner(id, canvas, request_next_frame);
        if t0.is_some() {
            crate::perf::end(&format!("present[{id}]"), t0);
            if ok {
                crate::perf::tick(&format!("interval[{id}]"));
            }
            crate::perf::flush_if_due();
        }
        ok
    }

    /// Whether `id` should be drawn now. Every `present` asks the compositor for a frame callback,
    /// and until it arrives the compositor has not shown the previous frame yet, so drawing another
    /// one is wasted work. A surface that is not being shown gets no callbacks at all; after
    /// `STALE_FRAME` it is drawn anyway so its content is current when it reappears.
    pub fn frame_ready(&self, id: SurfaceId) -> bool {
        const STALE_FRAME: std::time::Duration = std::time::Duration::from_millis(250);
        match self.state.surfaces.get(&id) {
            Some(ms) => {
                !ms.frame_pending
                    || ms.frame_sent_at.map(|t| t.elapsed() > STALE_FRAME).unwrap_or(true)
            }
            None => false,
        }
    }

    /// Tell the compositor where this frame's glass is, so it blurs what is behind exactly those
    /// shapes (ext-background-effect-v1). The region is pending state: it takes effect with the
    /// commit that presents the frame.
    fn sync_blur_region(state: &mut WlState, qh: &QueueHandle<WlState>, id: SurfaceId, canvas: &Canvas) {
        let (Some(mgr), Some(comp)) = (state.bg_effect_mgr.clone(), state.compositor.clone()) else {
            return;
        };
        let Some(ms) = state.surfaces.get_mut(&id) else {
            return;
        };
        if matches!(ms.role, RoleObj::Lock(_)) {
            return; // niri does not blur behind lock surfaces
        }
        let rects = blur_region_rects(&canvas.blur_rects);
        let sig = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            rects.hash(&mut h);
            h.finish() | 1
        };
        let sig = if rects.is_empty() { 0 } else { sig };
        if sig == ms.blur_sig {
            return;
        }
        if rects.is_empty() {
            if let Some(fx) = &ms.bg_effect {
                // Pass an explicit empty `wl_region` rather than `None`: in Niri, `None` unsets
                // the explicit subregion and falls back to blurring the entire layer geometry if
                // a `layer-rule` has `blur true`, whereas an empty region disables blur cleanly.
                let empty = comp.create_region(qh, ());
                fx.set_blur_region(Some(&empty));
                empty.destroy();
            }
        } else {
            let fx = ms
                .bg_effect
                .get_or_insert_with(|| mgr.get_background_effect(&ms.wl_surface, qh, ()));
            let region = comp.create_region(qh, ());
            for &(x, y, w, h) in &rects {
                region.add(x, y, w, h);
            }
            fx.set_blur_region(Some(&region));
            region.destroy();
        }
        ms.blur_sig = sig;
    }

    fn present_inner(&mut self, id: SurfaceId, canvas: &Canvas, _request_next_frame: bool) -> bool {
        match self.state.surfaces.get(&id) {
            Some(ms) if ms.configured && ms.logical_w != 0 && ms.logical_h != 0 => {}
            _ => return false,
        }
        Self::sync_blur_region(&mut self.state, &self.qh, id, canvas);
        let Some(ms) = self.state.surfaces.get_mut(&id) else {
            return false;
        };

        let allow_wsi = !ms.wsi_failed
            && std::env::var("TB520FU_VK_WSI")
                .map(|v| v != "0")
                .unwrap_or(true);

        if allow_wsi {
            if let Some(vk_mtx) = VkRenderer::global() {
                let is_hw = vk_mtx.lock().unwrap().is_hardware_gpu;
                if is_hw {
                    if let Some(vp) = &ms.viewport {
                        vp.set_destination(ms.logical_w as i32, ms.logical_h as i32);
                    }
                    if !ms.frame_pending {
                        ms.wl_surface.frame(&self.qh, id);
                        ms.frame_pending = true;
                    }
                    ms.frame_sent_at = Some(std::time::Instant::now());
                    let _ = self.conn.flush();

                    let wl_display_ptr =
                        self.conn.backend().display_ptr() as *mut std::ffi::c_void;
                    let wl_surface_ptr =
                        ms.wl_surface.id().as_ptr() as *mut std::ffi::c_void;

                    let mut vk = vk_mtx.lock().unwrap();
                    match vk.present_wayland_wsi(
                        id,
                        wl_display_ptr,
                        wl_surface_ptr,
                        canvas.phys_w,
                        canvas.phys_h,
                        canvas.clear_color,
                        canvas.surf_origin,
                        canvas.screen_phys_size,
                        &canvas.instances,
                    ) {
                        Ok(()) => {
                            ms.mapped = true;
                            let _ = self.conn.flush();
                            return true;
                        }
                        Err(_) => {
                            vk.destroy_wsi_surface(id);
                            ms.wsi_failed = true;
                        }
                    }
                }
            }
        }

        let slot_idx = if !ms.slots[0].busy {
            0
        } else if !ms.slots[1].busy {
            1
        } else {
            if !ms.frame_pending {
                ms.wl_surface.frame(&self.qh, id);
                ms.frame_pending = true;
                ms.frame_sent_at = Some(std::time::Instant::now());
                ms.wl_surface.commit();
                let _ = self.conn.flush();
            }
            return false;
        };

        let phys_w = canvas.phys_w;
        let phys_h = canvas.phys_h;
        let byte_len = (phys_w as usize) * (phys_h as usize) * 4;
        if ms.slots[slot_idx]
            .ensure_buffer(&self.qh, id, slot_idx, phys_w, phys_h)
            .is_err()
        {
            return false;
        }
        if !canvas.render_to_wayland_argb8888(ms.slots[slot_idx].slice_mut(byte_len)) {
            return false;
        }
        let buf = ms.slots[slot_idx].buffer.as_ref().unwrap();
        ms.slots[slot_idx].busy = true;

        if let Some(vp) = &ms.viewport {
            vp.set_destination(ms.logical_w as i32, ms.logical_h as i32);
        }
        if !ms.frame_pending {
            ms.wl_surface.frame(&self.qh, id);
            ms.frame_pending = true;
        }
        ms.frame_sent_at = Some(std::time::Instant::now());
        ms.wl_surface.attach(Some(buf), 0, 0);
        ms.wl_surface.damage_buffer(0, 0, phys_w as i32, phys_h as i32);
        ms.wl_surface.commit();
        ms.mapped = true;
        let _ = self.conn.flush();
        true
    }

    /// Directly fill and commit a static full-resolution `wl_shm` `Argb8888` (`BGRA`) buffer
    /// (used for `tb520fu-wallpaper` at `2944x1840` so crisp wallpapers allocate zero Vulkan swapchain/offscreen VRAM).
    pub fn present_shm_argb8888(
        &mut self,
        id: SurfaceId,
        phys_w: u32,
        phys_h: u32,
        fill_bgra: impl FnOnce(&mut [u8]),
    ) -> bool {
        let Some(ms) = self.state.surfaces.get_mut(&id) else {
            return false;
        };
        if !ms.configured || ms.logical_w == 0 || ms.logical_h == 0 || phys_w == 0 || phys_h == 0 {
            return false;
        }
        let slot_idx = if !ms.slots[0].busy {
            0
        } else if !ms.slots[1].busy {
            1
        } else {
            return false;
        };
        let byte_len = (phys_w as usize) * (phys_h as usize) * 4;
        if ms.slots[slot_idx]
            .ensure_buffer(&self.qh, id, slot_idx, phys_w, phys_h)
            .is_err()
        {
            return false;
        }
        fill_bgra(ms.slots[slot_idx].slice_mut(byte_len));
        let buf = ms.slots[slot_idx].buffer.as_ref().unwrap();
        ms.slots[slot_idx].busy = true;
        if let Some(vp) = &ms.viewport {
            vp.set_destination(ms.logical_w as i32, ms.logical_h as i32);
        }
        if !ms.frame_pending {
            ms.wl_surface.frame(&self.qh, id);
            ms.frame_pending = true;
        }
        ms.frame_sent_at = Some(std::time::Instant::now());
        ms.wl_surface.attach(Some(buf), 0, 0);
        ms.wl_surface.damage_buffer(0, 0, phys_w as i32, phys_h as i32);
        ms.wl_surface.commit();
        ms.mapped = true;
        let _ = self.conn.flush();
        true
    }

    /// Unmap a layer surface cleanly without destroying its D-Bus/Wayland state (`attach(None)`),
    /// tearing down any active WSI swapchain first so `attach(None)` is 100% protocol-clean.
    pub fn unmap_surface(&mut self, id: SurfaceId) {
        if let Some(ms) = self.state.surfaces.get_mut(&id) {
            if !ms.mapped {
                return;
            }
            // The swapchain stays (see `KEEP_SWAPCHAIN`). Its buffers are plain wl_buffers: detaching
            // the current one here just makes the compositor release it back to the driver.
            ms.unmapped_at = Some(std::time::Instant::now());
            ms.blur_sig = 0;
            ms.input_sig = 2;
            if let Some(fx) = &ms.bg_effect {
                if let Some(comp) = &self.state.compositor {
                    let empty = comp.create_region(&self.qh, ());
                    fx.set_blur_region(Some(&empty));
                    empty.destroy();
                } else {
                    fx.set_blur_region(None);
                }
            }
            ms.mapped = false;
            ms.configured = false;
            ms.frame_pending = false;

            // Never allow an unmapped layer surface to retain a positive `exclusive_zone`:
            // clear it both on the null-buffer unmap commit and on the post-unmap initial commit!
            if let (RoleObj::Layer(ls), Some(cfg)) = (&ms.role, &mut ms.layer_cfg) {
                cfg.exclusive_zone = cfg.exclusive_zone.min(0);
                ls.set_exclusive_zone(cfg.exclusive_zone);
            }
            ms.wl_surface.attach(None, 0, 0);
            ms.wl_surface.commit();

            if let (RoleObj::Layer(ls), Some(cfg)) = (&ms.role, ms.layer_cfg) {
                ls.set_layer(cfg.layer.to_wlr());
                ls.set_anchor(cfg.anchor_bits());
                ls.set_size(cfg.width, cfg.height);
                ls.set_exclusive_zone(cfg.exclusive_zone);
                ls.set_margin(
                    cfg.margin_top,
                    cfg.margin_right,
                    cfg.margin_bottom,
                    cfg.margin_left,
                );
                ls.set_keyboard_interactivity(cfg.kb_focus.to_wlr());
                if let Some(comp) = &self.state.compositor {
                    let region = comp.create_region(&self.qh, ());
                    ms.wl_surface.set_input_region(Some(&region));
                    region.destroy();
                }
                ms.wl_surface.commit();
            }
            let _ = self.conn.flush();
        }
    }

    // ---- Session Lock (`ext-session-lock-v1`) ----

    pub fn lock_session(&mut self) -> Result<Vec<SurfaceId>, String> {
        if self.state.session_lock.is_some() {
            return Ok(Vec::new());
        }
        let mgr = self
            .state
            .lock_mgr
            .as_ref()
            .ok_or("compositor does not advertise ext_session_lock_manager_v1")?;
        let comp = self.state.compositor.as_ref().ok_or("missing wl_compositor")?;
        let shm = self.state.shm.as_ref().ok_or("missing wl_shm")?;

        let lock = mgr.lock(&self.qh, ());
        let mut ids = Vec::new();
        let outputs = self.state.outputs.clone();
        for out in &outputs {
            let id = self.state.next_id;
            self.state.next_id += 1;
            let wl_surface = comp.create_surface(&self.qh, id);
            self.state
                .wl_id_to_surf
                .insert(wl_surface.id().protocol_id(), id);
            let lock_surf = lock.get_lock_surface(&wl_surface, &out.output, &self.qh, id);
            let viewport = self
                .state
                .viewporter
                .as_ref()
                .map(|vp| vp.get_viewport(&wl_surface, &self.qh, ()));
            let frac_scale = self
                .state
                .frac_mgr
                .as_ref()
                .map(|fm| fm.get_fractional_scale(&wl_surface, &self.qh, id));

            let s0 = ShmSlot::new(shm, &self.qh, id, 0, 1024 * 1024).map_err(|e| e.to_string())?;
            let s1 = ShmSlot::new(shm, &self.qh, id, 1, 1024 * 1024).map_err(|e| e.to_string())?;
            self.state.surfaces.insert(
                id,
                ManagedSurface {
                    wl_surface,
                    role: RoleObj::Lock(lock_surf),
                    layer_cfg: None,
                    viewport,
                    frac_scale,
                    logical_w: 0,
                    logical_h: 0,
                    scale_120: 120,
                    configured: false,
                    mapped: false,
                    bg_effect: None,
                    blur_sig: 0,
                    input_sig: 0,
                    frame_pending: false,
                    frame_sent_at: None,
                    unmapped_at: None,
                    wsi_failed: false,
                    keep_swapchain: false,
                    slots: [s0, s1],
                },
            );
            ids.push(id);
        }
        self.state.session_lock = Some(lock);
        let _ = self.conn.flush();
        Ok(ids)
    }

    pub fn unlock_session(&mut self, lock_surfaces: &[SurfaceId]) {
        if let Some(lock) = self.state.session_lock.take() {
            lock.unlock_and_destroy();
        }
        for &id in lock_surfaces {
            self.destroy_surface(id);
        }
        let _ = self.conn.flush();
    }

    // ---- Virtual Keyboard (`zwp_virtual_keyboard_v1`) ----

    pub fn ensure_virtual_keyboard(&mut self) -> bool {
        if self.state.vk.is_some() {
            return true;
        }
        let (Some(mgr), Some(seat)) = (&self.state.vk_mgr, &self.state.seat) else {
            return false;
        };
        let vk = mgr.create_virtual_keyboard(seat, &self.qh, ());
        let keymap_bytes = MINIMAL_US_XKB_KEYMAP.as_bytes();
        let cname = CString::new("tb520fu-vk-keymap").unwrap();
        let raw = unsafe { libc::memfd_create(cname.as_ptr(), libc::MFD_CLOEXEC) };
        if raw < 0 {
            return false;
        }
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        let total = keymap_bytes.len() + 1;
        unsafe {
            let _ = libc::ftruncate(raw, total as libc::off_t);
            let _ = libc::write(raw, keymap_bytes.as_ptr() as *const libc::c_void, keymap_bytes.len());
            let nul = 0u8;
            let _ = libc::write(raw, &nul as *const u8 as *const libc::c_void, 1);
        }
        vk.keymap(1, fd.as_fd(), total as u32);
        self.state.vk = Some(vk);
        let _ = self.conn.flush();
        true
    }

    /// Inject a raw evdev key press + release with optional XKB modifier mask (`1 = Shift`, `4 = Ctrl`, `8 = Alt`).
    pub fn send_vk_combo(&mut self, evdev_code: u32, mods_mask: u32) {
        if !self.ensure_virtual_keyboard() {
            return;
        }
        let Some(vk) = &self.state.vk else { return };
        let now_ms = (crate::touch_gate::TouchGateShm::monotonic_ns() / 1_000_000) as u32;
        if mods_mask != self.state.vk_mods {
            vk.modifiers(mods_mask, 0, 0, 0);
            self.state.vk_mods = mods_mask;
        }
        vk.key(now_ms, evdev_code, 1);
        vk.key(now_ms.wrapping_add(2), evdev_code, 0);
        if mods_mask != 0 {
            vk.modifiers(0, 0, 0, 0);
            self.state.vk_mods = 0;
        }
        let _ = self.conn.flush();
    }
}

const MINIMAL_US_XKB_KEYMAP: &str = r#"xkb_keymap {
xkb_keycodes "evdev" {
    minimum = 8;
    maximum = 255;
    <ESC> = 9; <AE01> = 10; <AE02> = 11; <AE03> = 12; <AE04> = 13; <AE05> = 14;
    <AE06> = 15; <AE07> = 16; <AE08> = 17; <AE09> = 18; <AE10> = 19; <AE11> = 20; <AE12> = 21;
    <BKSP> = 22; <TAB> = 23;
    <AD01> = 24; <AD02> = 25; <AD03> = 26; <AD04> = 27; <AD05> = 28;
    <AD06> = 29; <AD07> = 30; <AD08> = 31; <AD09> = 32; <AD10> = 33; <AD11> = 34; <AD12> = 35;
    <RTRN> = 36; <LCTL> = 37;
    <AC01> = 38; <AC02> = 39; <AC03> = 40; <AC04> = 41; <AC05> = 42;
    <AC06> = 43; <AC07> = 44; <AC08> = 45; <AC09> = 46; <AC10> = 47; <AC11> = 48; <TLDE> = 49;
    <LFSH> = 50; <BKSL> = 51;
    <AB01> = 52; <AB02> = 53; <AB03> = 54; <AB04> = 55; <AB05> = 56;
    <AB06> = 57; <AB07> = 58; <AB08> = 59; <AB09> = 60; <AB10> = 61;
    <RTSH> = 62; <LALT> = 64; <SPCE> = 65;
    <HOME> = 110; <UP> = 111; <PGUP> = 112; <LEFT> = 113; <RGHT> = 114; <END> = 115; <DOWN> = 116; <PGDN> = 117; <DELE> = 119;
};
xkb_types "minimal" {
    virtual_modifiers NumLock;
    type "ONE_LEVEL" { modifiers = none; level_name[Level1] = "Any"; };
    type "TWO_LEVEL" { modifiers = Shift; map[Shift] = Level2; level_name[Level1] = "Base"; level_name[Level2] = "Shift"; };
};
xkb_compatibility "minimal" {
    interpret Any + AnyOf(all) { action = SetMods(modifiers=modMapMods); };
};
xkb_symbols "us" {
    name[group1]="English (US)";
    key <ESC>  { [ Escape ] };
    key <AE01> { [ 1, exclam ] }; key <AE02> { [ 2, at ] }; key <AE03> { [ 3, numbersign ] };
    key <AE04> { [ 4, dollar ] }; key <AE05> { [ 5, percent ] }; key <AE06> { [ 6, asciicircum ] };
    key <AE07> { [ 7, ampersand ] }; key <AE08> { [ 8, asterisk ] }; key <AE09> { [ 9, parenleft ] };
    key <AE10> { [ 0, parenright ] }; key <AE11> { [ minus, underscore ] }; key <AE12> { [ equal, plus ] };
    key <BKSP> { [ BackSpace ] }; key <TAB>  { [ Tab, ISO_Left_Tab ] };
    key <AD01> { [ q, Q ] }; key <AD02> { [ w, W ] }; key <AD03> { [ e, E ] }; key <AD04> { [ r, R ] }; key <AD05> { [ t, T ] };
    key <AD06> { [ y, Y ] }; key <AD07> { [ u, U ] }; key <AD08> { [ i, I ] }; key <AD09> { [ o, O ] }; key <AD10> { [ p, P ] };
    key <AD11> { [ bracketleft, braceleft ] }; key <AD12> { [ bracketright, braceright ] };
    key <RTRN> { [ Return ] };
    key <AC01> { [ a, A ] }; key <AC02> { [ s, S ] }; key <AC03> { [ d, D ] }; key <AC04> { [ f, F ] }; key <AC05> { [ g, G ] };
    key <AC06> { [ h, H ] }; key <AC07> { [ j, J ] }; key <AC08> { [ k, K ] }; key <AC09> { [ l, L ] };
    key <AC10> { [ semicolon, colon ] }; key <AC11> { [ apostrophe, quotedbl ] }; key <TLDE> { [ grave, asciitilde ] };
    key <BKSL> { [ backslash, bar ] };
    key <AB01> { [ z, Z ] }; key <AB02> { [ x, X ] }; key <AB03> { [ c, C ] }; key <AB04> { [ v, V ] }; key <AB05> { [ b, B ] };
    key <AB06> { [ n, N ] }; key <AB07> { [ m, M ] }; key <AB08> { [ comma, less ] }; key <AB09> { [ period, greater ] }; key <AB10> { [ slash, question ] };
    key <SPCE> { [ space ] };
    key <HOME> { [ Home ] }; key <UP> { [ Up ] }; key <PGUP> { [ Prior ] };
    key <LEFT> { [ Left ] }; key <RGHT> { [ Right ] }; key <END> { [ End ] }; key <DOWN> { [ Down ] }; key <PGDN> { [ Next ] }; key <DELE> { [ Delete ] };
    key <LFSH> { [ Shift_L ] }; key <RTSH> { [ Shift_R ] }; key <LCTL> { [ Control_L ] }; key <LALT> { [ Alt_L ] };
    modifier_map Shift { <LFSH>, <RTSH> };
    modifier_map Control { <LCTL> };
    modifier_map Mod1 { <LALT> };
};
};"#;

// ---------------- Wayland Dispatch Implementations ----------------

impl Dispatch<wl_registry::WlRegistry, ()> for WlState {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_registry::Event::Global {
                name,
                interface,
                version,
            } => match interface.as_str() {
                "wl_compositor" => {
                    state.compositor = Some(registry.bind(name, version.min(6), qh, ()));
                }
                "wl_shm" => {
                    state.shm = Some(registry.bind(name, version.min(1), qh, ()));
                }
                "zwlr_layer_shell_v1" => {
                    state.layer_shell = Some(registry.bind(name, version.min(4), qh, ()));
                }
                "wp_viewporter" => {
                    state.viewporter = Some(registry.bind(name, version.min(1), qh, ()));
                }
                "ext_background_effect_manager_v1" => {
                    state.bg_effect_mgr = Some(registry.bind(name, version.min(1), qh, ()));
                }
                "wp_fractional_scale_manager_v1" => {
                    state.frac_mgr = Some(registry.bind(name, version.min(1), qh, ()));
                }
                "ext_session_lock_manager_v1" => {
                    state.lock_mgr = Some(registry.bind(name, version.min(1), qh, ()));
                }
                "zwp_virtual_keyboard_manager_v1" => {
                    state.vk_mgr = Some(registry.bind(name, version.min(1), qh, ()));
                }
                "wp_cursor_shape_manager_v1" => {
                    let mgr: wp_cursor_shape_manager_v1::WpCursorShapeManagerV1 =
                        registry.bind(name, version.min(1), qh, ());
                    if let Some(ptr) = &state.pointer {
                        if state.cursor_shape_dev.is_none() {
                            state.cursor_shape_dev = Some(mgr.get_pointer(ptr, qh, ()));
                        }
                    }
                    state.cursor_shape_mgr = Some(mgr);
                }
                "wl_seat" => {
                    if state.seat.is_none() {
                        state.seat = Some(registry.bind(name, version.min(8), qh, ()));
                    }
                }
                "wl_output" => {
                    let idx = state.outputs.len();
                    let output = registry.bind::<wl_output::WlOutput, _, _>(name, version.min(4), qh, idx);
                    state.outputs.push(OutputInfo {
                        global_name: name,
                        output,
                        name: format!("output-{idx}"),
                    });
                }
                _ => {}
            },
            wl_registry::Event::GlobalRemove { name } => {
                if let Some(pos) = state.outputs.iter().position(|o| o.global_name == name) {
                    let removed = state.outputs.remove(pos);
                    removed.output.release();
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_output::WlOutput, usize> for WlState {
    fn event(
        state: &mut Self,
        _proxy: &wl_output::WlOutput,
        event: wl_output::Event,
        idx: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_output::Event::Name { name } = event {
            if let Some(o) = state.outputs.get_mut(*idx) {
                o.name = name;
            }
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for WlState {
    fn event(
        state: &mut Self,
        seat: &wl_seat::WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(caps),
        } = event
        {
            if caps.contains(wl_seat::Capability::Touch) && state.touch.is_none() {
                state.touch = Some(seat.get_touch(qh, ()));
            }
            if caps.contains(wl_seat::Capability::Pointer) && state.pointer.is_none() {
                let ptr = seat.get_pointer(qh, ());
                if let Some(mgr) = &state.cursor_shape_mgr {
                    if state.cursor_shape_dev.is_none() {
                        state.cursor_shape_dev = Some(mgr.get_pointer(&ptr, qh, ()));
                    }
                }
                state.pointer = Some(ptr);
            }
            if caps.contains(wl_seat::Capability::Keyboard) && state.keyboard.is_none() {
                state.keyboard = Some(seat.get_keyboard(qh, ()));
            }
        }
    }
}

impl Dispatch<zwlr_layer_surface_v1::ZwlrLayerSurfaceV1, SurfaceId> for WlState {
    fn event(
        state: &mut Self,
        proxy: &zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        id: &SurfaceId,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_layer_surface_v1::Event::Configure {
                serial,
                width,
                height,
            } => {
                proxy.ack_configure(serial);
                if let Some(ms) = state.surfaces.get_mut(id) {
                    if width > 0 {
                        ms.logical_w = width;
                    }
                    if height > 0 {
                        ms.logical_h = height;
                    }
                    ms.configured = true;
                    let scale = (ms.scale_120 as f32) / 120.0;
                    state.events.push(UiEvent::Configured {
                        surface: *id,
                        width: ms.logical_w,
                        height: ms.logical_h,
                        scale,
                    });
                }
            }
            zwlr_layer_surface_v1::Event::Closed => {
                state.events.push(UiEvent::Closed { surface: *id });
            }
            _ => {}
        }
    }
}

impl Dispatch<ext_session_lock_v1::ExtSessionLockV1, ()> for WlState {
    fn event(
        state: &mut Self,
        _: &ext_session_lock_v1::ExtSessionLockV1,
        event: ext_session_lock_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ext_session_lock_v1::Event::Locked => state.events.push(UiEvent::SessionLocked),
            ext_session_lock_v1::Event::Finished => state.events.push(UiEvent::SessionLockFinished),
            _ => {}
        }
    }
}

impl Dispatch<ext_session_lock_surface_v1::ExtSessionLockSurfaceV1, SurfaceId> for WlState {
    fn event(
        state: &mut Self,
        proxy: &ext_session_lock_surface_v1::ExtSessionLockSurfaceV1,
        event: ext_session_lock_surface_v1::Event,
        id: &SurfaceId,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let ext_session_lock_surface_v1::Event::Configure {
            serial,
            width,
            height,
        } = event
        {
            proxy.ack_configure(serial);
            if let Some(ms) = state.surfaces.get_mut(id) {
                ms.logical_w = width;
                ms.logical_h = height;
                ms.configured = true;
                let scale = (ms.scale_120 as f32) / 120.0;
                state.events.push(UiEvent::Configured {
                    surface: *id,
                    width,
                    height,
                    scale,
                });
            }
        }
    }
}

impl Dispatch<wp_fractional_scale_v1::WpFractionalScaleV1, SurfaceId> for WlState {
    fn event(
        state: &mut Self,
        _: &wp_fractional_scale_v1::WpFractionalScaleV1,
        event: wp_fractional_scale_v1::Event,
        id: &SurfaceId,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event {
            if let Some(ms) = state.surfaces.get_mut(id) {
                ms.scale_120 = scale.max(60);
                if ms.configured {
                    state.events.push(UiEvent::Configured {
                        surface: *id,
                        width: ms.logical_w,
                        height: ms.logical_h,
                        scale: (ms.scale_120 as f32) / 120.0,
                    });
                }
            }
        }
    }
}

impl Dispatch<wl_callback::WlCallback, SurfaceId> for WlState {
    fn event(
        state: &mut Self,
        _: &wl_callback::WlCallback,
        event: wl_callback::Event,
        id: &SurfaceId,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_callback::Event::Done { callback_data } = event {
            if let Some(ms) = state.surfaces.get_mut(id) {
                ms.frame_pending = false;
            }
            state.events.push(UiEvent::Frame {
                surface: *id,
                time_ms: callback_data,
            });
        }
    }
}

impl Dispatch<wl_buffer::WlBuffer, (SurfaceId, usize)> for WlState {
    fn event(
        state: &mut Self,
        _: &wl_buffer::WlBuffer,
        event: wl_buffer::Event,
        (surf_id, slot_idx): &(SurfaceId, usize),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_buffer::Event::Release = event {
            if let Some(ms) = state.surfaces.get_mut(surf_id) {
                if let Some(slot) = ms.slots.get_mut(*slot_idx) {
                    slot.busy = false;
                }
            }
        }
    }
}

impl Dispatch<wl_touch::WlTouch, ()> for WlState {
    fn event(
        state: &mut Self,
        _: &wl_touch::WlTouch,
        event: wl_touch::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_touch::Event::Down {
                time,
                surface,
                id,
                x,
                y,
                ..
            } => {
                if let Some(&sid) = state.wl_id_to_surf.get(&surface.id().protocol_id()) {
                    state.touch_focus.insert(id, sid);
                    state.events.push(UiEvent::TouchDown {
                        surface: sid,
                        slot: id,
                        x: x as f32,
                        y: y as f32,
                        time_ms: time,
                    });
                }
            }
            wl_touch::Event::Motion { time, id, x, y } => {
                if let Some(&sid) = state.touch_focus.get(&id) {
                    state.events.push(UiEvent::TouchMotion {
                        surface: sid,
                        slot: id,
                        x: x as f32,
                        y: y as f32,
                        time_ms: time,
                    });
                }
            }
            wl_touch::Event::Up { time, id, .. } => {
                if let Some(sid) = state.touch_focus.remove(&id) {
                    state.events.push(UiEvent::TouchUp {
                        surface: sid,
                        slot: id,
                        time_ms: time,
                    });
                }
            }
            wl_touch::Event::Cancel => {
                let mut seen = Vec::new();
                for (_, sid) in state.touch_focus.drain() {
                    if !seen.contains(&sid) {
                        seen.push(sid);
                        state.events.push(UiEvent::TouchCancel { surface: sid });
                    }
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_pointer::WlPointer, ()> for WlState {
    fn event(
        state: &mut Self,
        _: &wl_pointer::WlPointer,
        event: wl_pointer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_pointer::Event::Enter {
                serial,
                surface,
                surface_x,
                surface_y,
            } => {
                if let Some(dev) = &state.cursor_shape_dev {
                    dev.set_shape(serial, wp_cursor_shape_device_v1::Shape::Default);
                }
                state.pointer_focus = state.wl_id_to_surf.get(&surface.id().protocol_id()).copied();
                state.pointer_pos = (surface_x as f32, surface_y as f32);
            }
            wl_pointer::Event::Leave { .. } => {
                if state.pointer_down {
                    if let Some(sid) = state.pointer_focus {
                        state.events.push(UiEvent::TouchUp {
                            surface: sid,
                            slot: -1,
                            time_ms: 0,
                        });
                    }
                    state.pointer_down = false;
                }
                state.pointer_focus = None;
            }
            wl_pointer::Event::Motion {
                time,
                surface_x,
                surface_y,
            } => {
                state.pointer_pos = (surface_x as f32, surface_y as f32);
                if let Some(sid) = state.pointer_focus {
                    if state.pointer_down {
                        state.events.push(UiEvent::TouchMotion {
                            surface: sid,
                            slot: -1,
                            x: surface_x as f32,
                            y: surface_y as f32,
                            time_ms: time,
                        });
                    } else {
                        let now = std::time::Instant::now();
                        let due = state
                            .last_pointer_activity
                            .map(|t| now.duration_since(t).as_millis() >= 150)
                            .unwrap_or(true);
                        if due {
                            state.last_pointer_activity = Some(now);
                            state.events.push(UiEvent::PointerActivity { surface: sid });
                        }
                    }
                }
            }
            wl_pointer::Event::Button {
                time,
                button,
                state: WEnum::Value(btn_state),
                ..
            } => {
                if button == 0x110 {
                    // BTN_LEFT -> synthetic touch slot -1
                    if let Some(sid) = state.pointer_focus {
                        if btn_state == wl_pointer::ButtonState::Pressed {
                            state.pointer_down = true;
                            state.events.push(UiEvent::TouchDown {
                                surface: sid,
                                slot: -1,
                                x: state.pointer_pos.0,
                                y: state.pointer_pos.1,
                                time_ms: time,
                            });
                        } else if state.pointer_down {
                            state.pointer_down = false;
                            state.events.push(UiEvent::TouchUp {
                                surface: sid,
                                slot: -1,
                                time_ms: time,
                            });
                        }
                    }
                } else if (button == 0x111 || button == 0x112)
                    && btn_state == wl_pointer::ButtonState::Pressed
                {
                    if let Some(sid) = state.pointer_focus {
                        state.events.push(UiEvent::PointerSecondaryClick {
                            surface: sid,
                            x: state.pointer_pos.0,
                            y: state.pointer_pos.1,
                        });
                    }
                }
            }
            wl_pointer::Event::Axis {
                axis: WEnum::Value(axis),
                value,
                ..
            } => {
                if let Some(sid) = state.pointer_focus {
                    let (dx, dy) = match axis {
                        wl_pointer::Axis::HorizontalScroll => (value as f32, 0.0),
                        wl_pointer::Axis::VerticalScroll => (0.0, value as f32),
                        _ => (0.0, 0.0),
                    };
                    state.events.push(UiEvent::Scroll {
                        surface: sid,
                        x: state.pointer_pos.0,
                        y: state.pointer_pos.1,
                        dx,
                        dy,
                    });
                }
            }
            wl_pointer::Event::AxisStop {
                axis:
                    WEnum::Value(
                        wl_pointer::Axis::VerticalScroll | wl_pointer::Axis::HorizontalScroll,
                    ),
                ..
            } => {
                if let Some(sid) = state.pointer_focus {
                    state.events.push(UiEvent::ScrollStop { surface: sid });
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for WlState {
    fn event(
        state: &mut Self,
        _: &wl_keyboard::WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_keyboard::Event::Key {
            key,
            state: WEnum::Value(ks),
            ..
        } = event
        {
            state.events.push(UiEvent::KeyPress {
                evdev_code: key,
                pressed: ks == wl_keyboard::KeyState::Pressed,
            });
        }
    }
}

/// Rounded glass shapes as integer rectangles for a `wl_region` (logical surface coordinates).
/// Corners become one-pixel-high strips; everything rounds inwards so blur never pokes out past the
/// antialiased edge of the glass.
fn blur_region_rects(shapes: &[crate::canvas::BlurRect]) -> Vec<(i32, i32, i32, i32)> {
    let mut out = Vec::new();
    for s in shapes {
        let x0 = s.rect.x.ceil() as i32;
        let y0 = s.rect.y.ceil() as i32;
        let x1 = (s.rect.x + s.rect.w).floor() as i32;
        let y1 = (s.rect.y + s.rect.h).floor() as i32;
        if x1 <= x0 || y1 <= y0 {
            continue;
        }
        let max_r = ((x1 - x0).min(y1 - y0) as f32) * 0.5;
        let [tl, tr, br, bl] = s.radii.map(|r| r.clamp(0.0, max_r));
        let top = tl.max(tr).ceil() as i32;
        let bottom = bl.max(br).ceil() as i32;
        // how far a corner of radius `r` cuts in on the row `dy` pixels from its edge
        let inset = |r: f32, dy: i32| -> i32 {
            let t = r - (dy as f32 + 0.5);
            if r <= 0.5 || t <= 0.0 {
                0
            } else {
                (r - (r * r - t * t).max(0.0).sqrt()).ceil() as i32
            }
        };
        for dy in 0..top {
            let (l, r) = (inset(tl, dy), inset(tr, dy));
            if x1 - x0 - l - r > 0 {
                out.push((x0 + l, y0 + dy, x1 - x0 - l - r, 1));
            }
        }
        if y1 - y0 - top - bottom > 0 {
            out.push((x0, y0 + top, x1 - x0, y1 - y0 - top - bottom));
        }
        for dy in 0..bottom {
            let (l, r) = (inset(bl, dy), inset(br, dy));
            if x1 - x0 - l - r > 0 {
                out.push((x0 + l, y1 - 1 - dy, x1 - x0 - l - r, 1));
            }
        }
    }
    out
}

// No-op dispatches for stateless protocol objects
macro_rules! noop_dispatch {
    ($ty:ty, $ud:ty) => {
        impl Dispatch<$ty, $ud> for WlState {
            fn event(
                _: &mut Self,
                _: &$ty,
                _: <$ty as Proxy>::Event,
                _: &$ud,
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
            }
        }
    };
}

noop_dispatch!(wl_compositor::WlCompositor, ());
noop_dispatch!(wl_surface::WlSurface, SurfaceId);
noop_dispatch!(wl_region::WlRegion, ());
noop_dispatch!(wl_shm::WlShm, ());
noop_dispatch!(wl_shm_pool::WlShmPool, ());
noop_dispatch!(ext_background_effect_manager_v1::ExtBackgroundEffectManagerV1, ());
noop_dispatch!(ext_background_effect_surface_v1::ExtBackgroundEffectSurfaceV1, ());
noop_dispatch!(zwlr_layer_shell_v1::ZwlrLayerShellV1, ());
noop_dispatch!(wp_viewporter::WpViewporter, ());
noop_dispatch!(wp_viewport::WpViewport, ());
noop_dispatch!(wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1, ());
noop_dispatch!(ext_session_lock_manager_v1::ExtSessionLockManagerV1, ());
noop_dispatch!(zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1, ());
noop_dispatch!(zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1, ());
noop_dispatch!(wp_cursor_shape_manager_v1::WpCursorShapeManagerV1, ());
noop_dispatch!(wp_cursor_shape_device_v1::WpCursorShapeDeviceV1, ());

#[allow(dead_code)]
fn _keep_borrowed_fd(_: BorrowedFd<'_>) {}

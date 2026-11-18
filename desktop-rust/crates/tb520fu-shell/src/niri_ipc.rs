// SPDX-License-Identifier: MIT
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::time::Duration;

use serde_json::Value;

use crate::sys_state::LaunchMode;

#[derive(Debug, Clone, Default)]
pub struct WorkspaceInfo {
    pub id: u64,
    pub idx: u32,
    pub output: String,
    pub is_active: bool,
    pub is_focused: bool,
}

#[derive(Debug, Clone, Default)]
pub struct WindowInfo {
    pub id: u64,
    pub title: String,
    pub app_id: String,
    pub workspace_id: Option<u64>,
    pub is_focused: bool,
    pub is_floating: bool,
    pub col_index: Option<usize>,
    pub row_index: Option<usize>,
    pub tile_pos: Option<(f64, f64)>,
    pub tile_width: f64,
    pub tile_height: f64,
}

#[derive(Debug, Clone)]
pub struct ScrollColumn {
    pub col_index: usize,
    pub width: f64,
    pub focused: bool,
    pub primary_win_id: u64,
    pub app_id: String,
    pub title: String,
    /// `(win_id, height, is_focused)` ordered top-to-bottom within this column.
    pub stack_windows: Vec<(u64, f64, bool)>,
}

#[derive(Debug, Clone)]
pub struct ExternalOutput {
    pub name: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, Default)]
pub struct NiriSnapshot {
    pub connected: bool,
    pub workspaces: Vec<WorkspaceInfo>,
    pub windows: Vec<WindowInfo>,
    pub overview: bool,
    pub external_dp: Option<ExternalOutput>,
}

impl NiriSnapshot {
    pub fn focused_window(&self) -> Option<&WindowInfo> {
        self.windows.iter().find(|w| w.is_focused)
    }

    pub fn primary_output_name(&self) -> &str {
        self.workspaces
            .iter()
            .find(|w| w.is_focused && !w.output.is_empty())
            .or_else(|| self.workspaces.iter().find(|w| w.is_active && !w.output.is_empty()))
            .map(|w| w.output.as_str())
            .unwrap_or("DSI-1")
    }

    pub fn active_workspace_on(&self, preferred_output: &str) -> Option<&WorkspaceInfo> {
        self.workspaces
            .iter()
            .find(|w| w.output == preferred_output && w.is_active)
            .or_else(|| self.workspaces.iter().find(|w| w.is_focused))
            .or_else(|| self.workspaces.iter().find(|w| w.is_active))
    }

    /// Returns `(dots, trailing_idx)` where each dot is `(ws_idx, is_active, is_trailing_empty)`.
    /// Always guarantees a permanent trailing hollow empty workspace `○` at the right end!
    pub fn workspace_dots(&self, preferred_output: &str) -> (Vec<(u32, bool, bool)>, u32) {
        let mut ws_list: Vec<&WorkspaceInfo> = self
            .workspaces
            .iter()
            .filter(|w| w.output.is_empty() || w.output == preferred_output)
            .collect();
        if ws_list.is_empty() {
            ws_list = self.workspaces.iter().collect();
        }
        ws_list.sort_by_key(|w| w.idx);

        if ws_list.is_empty() {
            return (vec![(1, true, false), (2, false, true)], 2);
        }

        let has_win = |ws_id: u64| self.windows.iter().any(|w| w.workspace_id == Some(ws_id));
        let mut dots: Vec<(u32, bool, bool)> = Vec::new();
        for w in &ws_list {
            let empty = !has_win(w.id);
            dots.push((w.idx, w.is_active, empty));
        }

        // Collapse multiple trailing empty non-active workspaces into one trailing `○`,
        // or append a synthetic trailing `○` if the last workspace is occupied or active!
        while dots.len() >= 2 {
            let last = dots[dots.len() - 1];
            let prev = dots[dots.len() - 2];
            if last.2 && !last.1 && prev.2 && !prev.1 {
                dots.pop();
            } else {
                break;
            }
        }

        let need_extra = match dots.last() {
            Some(&(_, is_act, is_empty)) => !is_empty || is_act,
            None => true,
        };
        if need_extra {
            let next_idx = dots.last().map(|d| d.0 + 1).unwrap_or(1);
            dots.push((next_idx, false, true));
        }

        // Only the final item is marked as the trailing empty `○` ring
        let last_pos = dots.len() - 1;
        for (i, d) in dots.iter_mut().enumerate() {
            d.2 = i == last_pos && !d.1;
        }
        let trailing_idx = dots[last_pos].0;
        (dots, trailing_idx)
    }

    /// Proportional scrolling columns of `ws_id`, ordered left-to-right, including vertical stacks.
    /// Floating windows and non-scrolling windows (`col_index == None`) are strictly excluded.
    pub fn columns_of(&self, ws_id: u64) -> Vec<ScrollColumn> {
        let mut map: BTreeMap<usize, (ScrollColumn, Vec<(usize, u64, f64, bool)>)> = BTreeMap::new();
        for w in &self.windows {
            if w.workspace_id != Some(ws_id) || w.is_floating {
                continue;
            }
            let Some(col_idx) = w.col_index else {
                continue;
            };
            let row_idx = w.row_index.unwrap_or(1);
            let width = w.tile_width.max(120.0);
            let height = w.tile_height.max(80.0);
            map.entry(col_idx)
                .and_modify(|(c, rows)| {
                    c.width = c.width.max(width);
                    if w.is_focused {
                        c.focused = true;
                        c.primary_win_id = w.id;
                        c.app_id = w.app_id.clone();
                        c.title = w.title.clone();
                    }
                    rows.push((row_idx, w.id, height, w.is_focused));
                })
                .or_insert_with(|| {
                    (
                        ScrollColumn {
                            col_index: col_idx,
                            width,
                            focused: w.is_focused,
                            primary_win_id: w.id,
                            app_id: w.app_id.clone(),
                            title: w.title.clone(),
                            stack_windows: Vec::new(),
                        },
                        vec![(row_idx, w.id, height, w.is_focused)],
                    )
                });
        }
        map.into_values()
            .map(|(mut c, mut rows)| {
                rows.sort_by_key(|r| r.0);
                c.stack_windows = rows.into_iter().map(|(_, id, h, f)| (id, h, f)).collect();
                c
            })
            .collect()
    }
}

fn find_socket_path() -> Option<String> {
    if let Ok(s) = std::env::var("NIRI_SOCKET") {
        if !s.is_empty() && std::path::Path::new(&s).exists() {
            return Some(s);
        }
    }
    let uid = unsafe { libc::getuid() };
    let dir = format!("/run/user/{uid}");
    let entries = std::fs::read_dir(dir).ok()?;
    for e in entries.flatten() {
        let name = e.file_name();
        let s = name.to_string_lossy();
        if s.starts_with("niri.") && s.ends_with(".sock") {
            return Some(e.path().to_string_lossy().into_owned());
        }
    }
    None
}

/// Send a single JSON request line to `$NIRI_SOCKET` and return the reply line (blocking; worker-only).
pub fn send_niri_request(json_line: &str) -> Option<String> {
    let sock = find_socket_path()?;
    let mut stream = UnixStream::connect(sock).ok()?;
    let _ = stream.set_read_timeout(Some(Duration::from_millis(600)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(600)));
    let mut line = json_line.trim().to_string();
    line.push('\n');
    stream.write_all(line.as_bytes()).ok()?;
    let mut reader = BufReader::new(stream);
    let mut reply = String::new();
    reader.read_line(&mut reply).ok()?;
    Some(reply)
}

enum NiriAsyncCmd {
    Json(String),
    FlushResize,
}

#[derive(Default)]
struct CoalescedResize {
    col_width_pct: Option<f64>,
    win_height: Option<(Option<u64>, f64)>,
    queued: bool,
}

static NIRI_CMD_TX: OnceLock<mpsc::Sender<NiriAsyncCmd>> = OnceLock::new();
static COALESCE_RESIZE: OnceLock<Mutex<CoalescedResize>> = OnceLock::new();

fn try_persistent_niri(
    conn: &mut Option<(UnixStream, BufReader<UnixStream>)>,
    json_line: &str,
) {
    let mut line = json_line.trim().to_string();
    line.push('\n');

    if let Some((ref mut s, ref mut r)) = conn {
        if s.write_all(line.as_bytes()).is_ok() && s.flush().is_ok() {
            let mut reply = String::new();
            if r.read_line(&mut reply).is_ok_and(|n| n > 0) {
                return;
            }
        }
    }
    *conn = None;

    let Some(sock) = find_socket_path() else {
        return;
    };
    let Ok(mut s) = UnixStream::connect(sock) else {
        return;
    };
    let _ = s.set_read_timeout(Some(Duration::from_millis(500)));
    let _ = s.set_write_timeout(Some(Duration::from_millis(500)));
    let Ok(s_clone) = s.try_clone() else {
        let _ = s.write_all(line.as_bytes());
        return;
    };
    let mut r = BufReader::new(s_clone);
    if s.write_all(line.as_bytes()).is_ok() && s.flush().is_ok() {
        let mut reply = String::new();
        if r.read_line(&mut reply).is_ok_and(|n| n > 0) {
            *conn = Some((s, r));
        }
    }
}

fn niri_cmd_sender() -> &'static mpsc::Sender<NiriAsyncCmd> {
    NIRI_CMD_TX.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<NiriAsyncCmd>();
        let _ = std::thread::Builder::new()
            .name("tb520fu-niri-cmd".into())
            .spawn(move || {
                let mut conn: Option<(UnixStream, BufReader<UnixStream>)> = None;
                while let Ok(cmd) = rx.recv() {
                    match cmd {
                        NiriAsyncCmd::Json(req) => {
                            try_persistent_niri(&mut conn, &req);
                        }
                        NiriAsyncCmd::FlushResize => {
                            let (cw, wh) = if let Some(m) = COALESCE_RESIZE.get() {
                                let mut g = m.lock().unwrap();
                                g.queued = false;
                                (g.col_width_pct.take(), g.win_height.take())
                            } else {
                                (None, None)
                            };
                            if let Some(p) = cw {
                                try_persistent_niri(
                                    &mut conn,
                                    &format!(
                                        "{{\"Action\":{{\"SetColumnWidth\":{{\"change\":{{\"SetProportion\":{p:.2}}}}}}}}}"
                                    ),
                                );
                            }
                            if let Some((win_id, p)) = wh {
                                let id_json = match win_id {
                                    Some(id) => id.to_string(),
                                    None => "null".into(),
                                };
                                try_persistent_niri(
                                    &mut conn,
                                    &format!(
                                        "{{\"Action\":{{\"SetWindowHeight\":{{\"id\":{id_json},\"change\":{{\"SetProportion\":{p:.2}}}}}}}}}"
                                    ),
                                );
                            }
                        }
                    }
                }
            });
        tx
    })
}

fn enqueue_niri_json(req: String) {
    let _ = niri_cmd_sender().send(NiriAsyncCmd::Json(req));
}

pub fn toggle_overview() {
    crate::sys_state::request_gpu_boost(650);
    enqueue_niri_json("{\"Action\":{\"ToggleOverview\":{}}}".into());
}

pub fn focus_window(id: u64) {
    crate::sys_state::request_gpu_boost(450);
    enqueue_niri_json(format!("{{\"Action\":{{\"FocusWindow\":{{\"id\":{id}}}}}}}"));
}

pub fn close_window(id: u64) {
    crate::sys_state::request_gpu_boost(450);
    enqueue_niri_json(format!("{{\"Action\":{{\"CloseWindow\":{{\"id\":{id}}}}}}}"));
}

pub fn focus_workspace_idx(idx: u8) {
    crate::sys_state::request_gpu_boost(550);
    enqueue_niri_json(format!(
        "{{\"Action\":{{\"FocusWorkspace\":{{\"reference\":{{\"Index\":{idx}}}}}}}}}"
    ));
}

pub fn focus_trailing_empty_workspace(niri: &NiriSnapshot) {
    let (_, trailing_idx) = niri.workspace_dots("DSI-1");
    focus_workspace_idx(trailing_idx.min(250) as u8);
}

/// Set focused column width as percentage (`18.0..=100.0`), coalesced across 240Hz touch frames.
pub fn set_column_width_pct(pct: f64) {
    let p = pct.clamp(18.0, 100.0);
    let slot = COALESCE_RESIZE.get_or_init(|| Mutex::new(CoalescedResize::default()));
    let should_send = {
        let mut g = slot.lock().unwrap();
        g.col_width_pct = Some(p);
        if !g.queued {
            g.queued = true;
            true
        } else {
            false
        }
    };
    if should_send {
        let _ = niri_cmd_sender().send(NiriAsyncCmd::FlushResize);
    }
}

/// Set window height inside a vertical stack as percentage (`18.0..=85.0`), coalesced across 240Hz touch frames.
pub fn set_window_height_pct(win_id: Option<u64>, pct: f64) {
    let p = pct.clamp(18.0, 85.0);
    let slot = COALESCE_RESIZE.get_or_init(|| Mutex::new(CoalescedResize::default()));
    let should_send = {
        let mut g = slot.lock().unwrap();
        g.win_height = Some((win_id, p));
        if !g.queued {
            g.queued = true;
            true
        } else {
            false
        }
    };
    if should_send {
        let _ = niri_cmd_sender().send(NiriAsyncCmd::FlushResize);
    }
}

pub fn consume_or_expel_left(win_id: Option<u64>) {
    let id_json = win_id.map(|i| i.to_string()).unwrap_or_else(|| "null".into());
    enqueue_niri_json(format!(
        "{{\"Action\":{{\"ConsumeOrExpelWindowLeft\":{{\"id\":{id_json}}}}}}}"
    ));
}

pub fn consume_or_expel_right(win_id: Option<u64>) {
    let id_json = win_id.map(|i| i.to_string()).unwrap_or_else(|| "null".into());
    enqueue_niri_json(format!(
        "{{\"Action\":{{\"ConsumeOrExpelWindowRight\":{{\"id\":{id_json}}}}}}}"
    ));
}

pub fn set_window_width_pct(win_id: Option<u64>, pct: f64) {
    crate::sys_state::request_gpu_boost(450);
    let p = pct.clamp(18.0, 100.0);
    let id_json = win_id.map(|i| i.to_string()).unwrap_or_else(|| "null".into());
    enqueue_niri_json(format!(
        "{{\"Action\":{{\"SetWindowWidth\":{{\"id\":{id_json},\"change\":{{\"SetProportion\":{p:.4}}}}}}}}}"
    ));
}

pub fn set_window_height_pct_id(win_id: Option<u64>, pct: f64) {
    crate::sys_state::request_gpu_boost(450);
    let p = pct.clamp(18.0, 95.0);
    let id_json = win_id.map(|i| i.to_string()).unwrap_or_else(|| "null".into());
    enqueue_niri_json(format!(
        "{{\"Action\":{{\"SetWindowHeight\":{{\"id\":{id_json},\"change\":{{\"SetProportion\":{p:.4}}}}}}}}}"
    ));
}

pub fn toggle_window_floating_id(win_id: u64) {
    crate::sys_state::request_gpu_boost(500);
    enqueue_niri_json(format!(
        "{{\"Action\":{{\"ToggleWindowFloating\":{{\"id\":{win_id}}}}}}}"
    ));
}

pub fn move_window_to_tiling_id(win_id: u64) {
    crate::sys_state::request_gpu_boost(500);
    enqueue_niri_json(format!(
        "{{\"Action\":{{\"MoveWindowToTiling\":{{\"id\":{win_id}}}}}}}"
    ));
}

pub fn center_window_id(win_id: u64) {
    crate::sys_state::request_gpu_boost(450);
    enqueue_niri_json(format!(
        "{{\"Action\":{{\"CenterWindow\":{{\"id\":{win_id}}}}}}}"
    ));
}

/// Apply an independent Niri column width preset (`20.0..=100.0`) from the Window Top-Pill Menu.
/// Per Niri's scrollable-strip design philosophy, changing a window's width preset only resizes
/// that window's column and never mutates neighbor columns (coupled resizing happens only when
/// explicitly dragging the split seam between two columns).
pub fn apply_split_preset_for_window(snap: &NiriSnapshot, win_id: u64, pct: f64) {
    if let Some(win) = snap.windows.iter().find(|w| w.id == win_id) {
        if win.is_floating {
            move_window_to_tiling_id(win_id);
        }
    }
    set_window_width_pct(Some(win_id), pct.clamp(20.0, 100.0));
}

/// Double-tap gutter reset: restore balanced `50%` column width and `50%` stack height.
pub fn reset_50_50() {
    set_column_width_pct(50.0);
    set_window_height_pct(None, 50.0);
}

pub fn spawn_command(argv: &[String]) {
    if argv.is_empty() {
        return;
    }
    crate::sys_state::request_gpu_boost(650);
    if let Ok(cmd_json) = serde_json::to_string(argv) {
        enqueue_niri_json(format!(
            "{{\"Action\":{{\"Spawn\":{{\"command\":{cmd_json}}}}}}}"
        ));
    }
}

pub fn spawn_app_with_mode(argv: Vec<String>, mode: LaunchMode, niri: &NiriSnapshot) {
    if argv.is_empty() {
        return;
    }
    let (_, trailing_idx) = niri.workspace_dots("DSI-1");
    let has_focused = niri.focused_window().is_some();
    std::thread::spawn(move || match mode {
        LaunchMode::NewCol => {
            spawn_command(&argv);
        }
        LaunchMode::NewWs => {
            focus_workspace_idx(trailing_idx.min(250) as u8);
            std::thread::sleep(Duration::from_millis(60));
            spawn_command(&argv);
        }
        LaunchMode::Stack => {
            spawn_command(&argv);
            if has_focused {
                std::thread::sleep(Duration::from_millis(260));
                consume_or_expel_left(None);
            }
        }
    });
}

/// Rotate `DSI-1` (`"Normal"`, `"_90"`, `"_180"`, `"_270"`).
pub fn set_dsi_transform(transform_variant: &str) {
    enqueue_niri_json(format!(
        "{{\"Output\":{{\"output\":\"DSI-1\",\"action\":{{\"Transform\":{{\"transform\":\"{transform_variant}\"}}}}}}}}"
    ));
}

/// Switch `DSI-1` refresh rate to the closest supported `2944x1840` mode (`144Hz`/`120Hz` high vs `60Hz` power-save).
pub fn set_dsi_refresh_hz(desired_hz: f64) {
    let mut best_hz = desired_hz;
    if let Some(reply) = send_niri_request("\"Outputs\"") {
        if let Ok(v) = serde_json::from_str::<Value>(&reply) {
            if let Some(modes) = v
                .get("Ok")
                .and_then(|o| o.get("Outputs"))
                .and_then(|o| o.get("DSI-1"))
                .and_then(|d| d.get("modes"))
                .and_then(|m| m.as_array())
            {
                let mut best_diff = f64::MAX;
                for m in modes {
                    let w = m.get("width").and_then(|x| x.as_u64()).unwrap_or(0);
                    let h = m.get("height").and_then(|x| x.as_u64()).unwrap_or(0);
                    let r_mhz = m.get("refresh_rate").and_then(|x| x.as_u64()).unwrap_or(0);
                    if w == 2944 && h == 1840 && r_mhz > 0 {
                        let hz = (r_mhz as f64) / 1000.0;
                        let diff = (hz - desired_hz).abs();
                        if diff < best_diff {
                            best_diff = diff;
                            best_hz = hz;
                        }
                    }
                }
            }
        }
    }
    let req = format!(
        "{{\"Output\":{{\"output\":\"DSI-1\",\"action\":{{\"Mode\":{{\"mode\":{{\"Specific\":{{\"width\":2944,\"height\":1840,\"refresh\":{best_hz:.3}}}}}}}}}}}}}"
    );
    let _ = send_niri_request(&req);
}

/// Enable or disable an external DisplayPort output (`DP-1`, `DP-2`, etc.).
pub fn set_output_power(output_name: &str, enable: bool) {
    let act = if enable { "\"On\"" } else { "\"Off\"" };
    enqueue_niri_json(format!(
        "{{\"Output\":{{\"output\":\"{output_name}\",\"action\":{act}}}}}"
    ));
}

pub fn power_off_monitors() {
    enqueue_niri_json("{\"Action\":{\"PowerOffMonitors\":{}}}".into());
}

pub fn power_on_monitors() {
    enqueue_niri_json("{\"Action\":{\"PowerOnMonitors\":{}}}".into());
}

/// Portrait Policy A: expand active columns to full width in portrait, restore `50%` split on landscape return.
pub fn apply_portrait_policy(is_portrait: bool) {
    let pct = if is_portrait { 100.0 } else { 50.0 };
    set_column_width_pct(pct);
}

pub fn query_external_dp() -> Option<ExternalOutput> {
    let reply = send_niri_request("\"Outputs\"")?;
    let v: Value = serde_json::from_str(&reply).ok()?;
    let outputs = v.get("Ok")?.get("Outputs")?.as_object()?;
    for (name, obj) in outputs {
        if name != "DSI-1" {
            let enabled = obj.get("logical").is_some_and(|l| !l.is_null());
            return Some(ExternalOutput {
                name: name.clone(),
                enabled,
            });
        }
    }
    None
}

fn parse_window(v: &Value) -> Option<WindowInfo> {
    let id = v.get("id")?.as_u64()?;
    let title = v
        .get("title")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let app_id = v
        .get("app_id")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let workspace_id = v.get("workspace_id").and_then(|x| x.as_u64());
    let is_focused = v.get("is_focused").and_then(|x| x.as_bool()).unwrap_or(false);
    let mut is_floating = v
        .get("is_floating")
        .and_then(|x| x.as_bool())
        .unwrap_or(false);
    let (mut col_index, mut row_index, mut tile_pos, mut tile_width, mut tile_height) =
        (None, None, None, 600.0, 960.0);
    if let Some(layout) = v.get("layout") {
        apply_layout(
            layout,
            &mut is_floating,
            &mut col_index,
            &mut row_index,
            &mut tile_pos,
            &mut tile_width,
            &mut tile_height,
        );
    }
    Some(WindowInfo {
        id,
        title,
        app_id,
        workspace_id,
        is_focused,
        is_floating,
        col_index,
        row_index,
        tile_pos,
        tile_width,
        tile_height,
    })
}

fn apply_layout(
    layout: &Value,
    is_floating: &mut bool,
    col_index: &mut Option<usize>,
    row_index: &mut Option<usize>,
    tile_pos: &mut Option<(f64, f64)>,
    tile_width: &mut f64,
    tile_height: &mut f64,
) {
    *col_index = None;
    *row_index = None;
    if let Some(pos) = layout.get("pos_in_scrolling_layout").and_then(|p| p.as_array()) {
        if let Some(c) = pos.first().and_then(|x| x.as_u64()) {
            *col_index = Some(c as usize);
        }
        if let Some(r) = pos.get(1).and_then(|x| x.as_u64()) {
            *row_index = Some(r as usize);
        }
        *is_floating = false;
    } else if layout.get("pos_in_scrolling_layout").is_some_and(|p| p.is_null()) {
        *is_floating = true;
    }
    *tile_pos = layout
        .get("tile_pos_in_workspace_view")
        .and_then(|p| p.as_array())
        .and_then(|arr| Some((arr.first()?.as_f64()?, arr.get(1)?.as_f64()?)));
    if let Some(sz) = layout.get("tile_size").and_then(|s| s.as_array()) {
        if let Some(w) = sz.first().and_then(|x| x.as_f64()) {
            *tile_width = w;
        }
        if let Some(h) = sz.get(1).and_then(|x| x.as_f64()) {
            *tile_height = h;
        }
    }
}

fn handle_event_line(snap: &mut NiriSnapshot, line: &str) -> (bool, bool) {
    let Ok(ev) = serde_json::from_str::<Value>(line) else {
        return (false, false);
    };
    if let Some(ws_arr) = ev
        .get("WorkspacesChanged")
        .and_then(|x| x.get("workspaces"))
        .and_then(|x| x.as_array())
    {
        snap.workspaces = ws_arr
            .iter()
            .filter_map(|w| {
                Some(WorkspaceInfo {
                    id: w.get("id")?.as_u64()?,
                    idx: w.get("idx").and_then(|x| x.as_u64()).unwrap_or(1) as u32,
                    output: w
                        .get("output")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string(),
                    is_active: w.get("is_active").and_then(|x| x.as_bool()).unwrap_or(false),
                    is_focused: w
                        .get("is_focused")
                        .and_then(|x| x.as_bool())
                        .unwrap_or(false),
                })
            })
            .collect();
        return (true, true);
    }
    if let Some(wa) = ev.get("WorkspaceActivated") {
        if let Some(id) = wa.get("id").and_then(|x| x.as_u64()) {
            let focused = wa.get("focused").and_then(|x| x.as_bool()).unwrap_or(false);
            let target_out = snap
                .workspaces
                .iter()
                .find(|w| w.id == id)
                .map(|w| w.output.clone());
            if let Some(out) = target_out {
                for w in &mut snap.workspaces {
                    if w.output == out {
                        w.is_active = w.id == id;
                    }
                    if focused {
                        w.is_focused = w.id == id;
                    }
                }
                return (true, false);
            }
        }
    }
    if let Some(win_arr) = ev
        .get("WindowsChanged")
        .and_then(|x| x.get("windows"))
        .and_then(|x| x.as_array())
    {
        snap.windows = win_arr.iter().filter_map(parse_window).collect();
        return (true, false);
    }
    if let Some(wobj) = ev.get("WindowOpenedOrChanged").and_then(|x| x.get("window")) {
        if let Some(win) = parse_window(wobj) {
            if win.is_focused {
                for existing in &mut snap.windows {
                    existing.is_focused = false;
                }
            }
            if let Some(slot) = snap.windows.iter_mut().find(|x| x.id == win.id) {
                *slot = win;
            } else {
                snap.windows.push(win);
            }
            return (true, false);
        }
    }
    if let Some(id) = ev
        .get("WindowClosed")
        .and_then(|x| x.get("id"))
        .and_then(|x| x.as_u64())
    {
        snap.windows.retain(|w| w.id != id);
        return (true, false);
    }
    if let Some(fc) = ev.get("WindowFocusChanged") {
        let id_opt = fc.get("id").and_then(|x| x.as_u64());
        for w in &mut snap.windows {
            w.is_focused = Some(w.id) == id_opt;
        }
        return (true, false);
    }
    if let Some(changes) = ev
        .get("WindowLayoutsChanged")
        .and_then(|x| x.get("changes"))
        .and_then(|x| x.as_array())
    {
        for pair in changes {
            if let Some(arr) = pair.as_array() {
                if arr.len() == 2 {
                    if let Some(id) = arr[0].as_u64() {
                        if let Some(w) = snap.windows.iter_mut().find(|w| w.id == id) {
                            apply_layout(
                                &arr[1],
                                &mut w.is_floating,
                                &mut w.col_index,
                                &mut w.row_index,
                                &mut w.tile_pos,
                                &mut w.tile_width,
                                &mut w.tile_height,
                            );
                        }
                    }
                }
            }
        }
        return (true, false);
    }
    if let Some(ov) = ev
        .get("OverviewOpenedOrClosed")
        .and_then(|x| x.get("is_open"))
        .and_then(|x| x.as_bool())
    {
        snap.overview = ov;
        crate::sys_state::request_gpu_boost(650);
        return (true, false);
    }
    if ev.get("ConfigLoaded").is_some() {
        return (true, true);
    }
    (false, false)
}

pub fn spawn_niri_listener(shared: Arc<Mutex<NiriSnapshot>>, wakeup_fd: Option<i32>) {
    std::thread::Builder::new()
        .name("tb520fu-niri-ipc".into())
        .spawn(move || {
            let wake = || {
                if let Some(fd) = wakeup_fd {
                    let one: u64 = 1;
                    unsafe {
                        let _ = libc::write(fd, &one as *const u64 as *const libc::c_void, 8);
                    }
                }
            };

            loop {
                let Some(sock) = find_socket_path() else {
                    std::thread::sleep(Duration::from_secs(2));
                    continue;
                };
                let Ok(mut stream) = UnixStream::connect(&sock) else {
                    std::thread::sleep(Duration::from_secs(2));
                    continue;
                };
                if stream.write_all(b"\"EventStream\"\n").is_err() {
                    std::thread::sleep(Duration::from_secs(2));
                    continue;
                }
                let dp = query_external_dp();
                {
                    let mut st = shared.lock().unwrap();
                    st.connected = true;
                    st.external_dp = dp;
                }
                wake();

                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                while reader.read_line(&mut line).is_ok_and(|n| n > 0) {
                    let (changed, need_dp) = {
                        let mut st = shared.lock().unwrap();
                        handle_event_line(&mut st, line.trim())
                    };
                    if need_dp {
                        let dp = query_external_dp();
                        shared.lock().unwrap().external_dp = dp;
                    }
                    if changed {
                        wake();
                    }
                    line.clear();
                }

                shared.lock().unwrap().connected = false;
                std::thread::sleep(Duration::from_secs(2));
            }
        })
        .ok();
}

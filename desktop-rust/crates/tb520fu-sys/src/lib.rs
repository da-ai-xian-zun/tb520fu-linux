// SPDX-License-Identifier: MIT
//! Decoupled pure-Rust system control and telemetry library for Lenovo TB520FU.
//!
//! Shared by `tb520fu-settings` and future CLI / voice / MCP tools without coupling
//! to any GUI toolkit or shell rendering loop.

pub mod audio;
pub mod bluetooth;
pub mod daemon;
pub mod default_apps;
pub mod niri_cfg;
pub mod osk_ime;
pub mod power_idle;
pub mod shell_prefs;
pub mod system_info;
pub mod wallpaper;
pub mod wifi;

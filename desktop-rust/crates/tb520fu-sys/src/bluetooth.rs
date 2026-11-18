// SPDX-License-Identifier: MIT
use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{mpsc, oneshot, Mutex};
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};

pub const AGENT_PATH: &str = "/fyi/tb520fu/SettingsAgent";

#[derive(Debug, Clone)]
pub struct BtDeviceItem {
    pub path: String,
    pub mac: String,
    pub name: String,
    pub kind_label: String,
    pub paired: bool,
    pub connected: bool,
    pub battery: Option<u8>,
}

#[derive(Debug, Clone, Default)]
pub struct BtState {
    pub enabled: bool,
    pub discovering: bool,
    pub paired: Vec<BtDeviceItem>,
    pub discovered: Vec<BtDeviceItem>,
}

#[derive(Debug)]
pub struct PairingPrompt {
    pub device_path: String,
    pub device_name: String,
    pub passkey: String,
    pub responder: oneshot::Sender<bool>,
}

fn looks_like_raw_mac(s: &str) -> bool {
    let clean: String = s.chars().filter(|c| *c != ':' && *c != '-').collect();
    clean.len() == 12 && clean.chars().all(|c| c.is_ascii_hexdigit())
}

fn classify_bt_kind(icon: &str, name: &str) -> &'static str {
    let nl = name.to_ascii_lowercase();
    if nl.contains("pen") || nl.contains("stylus") {
        "手写笔"
    } else if icon.contains("audio")
        || nl.contains("buds")
        || nl.contains("airpods")
        || nl.contains("inzone")
        || nl.contains("head")
    {
        "音频设备"
    } else if icon.contains("keyboard") || nl.contains("key") {
        "键盘"
    } else if icon.contains("mouse") {
        "鼠标"
    } else {
        "蓝牙外设"
    }
}

pub async fn get_bt_state(sys_bus: &zbus::Connection, pen_docked_level: Option<u8>) -> BtState {
    type ManagedMap = HashMap<OwnedObjectPath, HashMap<String, HashMap<String, OwnedValue>>>;
    let Ok(reply) = sys_bus
        .call_method(
            Some("org.bluez"),
            "/",
            Some("org.freedesktop.DBus.ObjectManager"),
            "GetManagedObjects",
            &(),
        )
        .await
    else {
        return BtState::default();
    };
    let Ok(objs) = reply.body().deserialize::<ManagedMap>() else {
        return BtState::default();
    };

    let mut enabled = false;
    let mut discovering = false;
    for ifaces in objs.values() {
        if let Some(adp) = ifaces.get("org.bluez.Adapter1") {
            enabled = adp
                .get("Powered")
                .and_then(|v| bool::try_from(v).ok())
                .unwrap_or(false);
            discovering = adp
                .get("Discovering")
                .and_then(|v| bool::try_from(v).ok())
                .unwrap_or(false);
            break;
        }
    }

    if !enabled {
        return BtState {
            enabled: false,
            discovering: false,
            paired: Vec::new(),
            discovered: Vec::new(),
        };
    }

    let mut paired = Vec::new();
    let mut discovered = Vec::new();

    for (path, ifaces) in &objs {
        let Some(dev) = ifaces.get("org.bluez.Device1") else {
            continue;
        };
        let mac = dev
            .get("Address")
            .and_then(|v| <&str>::try_from(v).ok())
            .unwrap_or("")
            .to_string();
        let is_paired = dev
            .get("Paired")
            .and_then(|v| bool::try_from(v).ok())
            .unwrap_or(false);
        let is_connected = dev
            .get("Connected")
            .and_then(|v| bool::try_from(v).ok())
            .unwrap_or(false);
        let raw_name = dev
            .get("Name")
            .and_then(|v| <&str>::try_from(v).ok())
            .unwrap_or("")
            .trim();
        let name = dev
            .get("Alias")
            .and_then(|v| <&str>::try_from(v).ok())
            .or_else(|| dev.get("Name").and_then(|v| <&str>::try_from(v).ok()))
            .unwrap_or("")
            .trim()
            .to_string();
        if name.is_empty() {
            continue;
        }
        let icon = dev
            .get("Icon")
            .and_then(|v| <&str>::try_from(v).ok())
            .unwrap_or("");
        let kind_label = classify_bt_kind(icon, &name).to_string();

        let mut battery = ifaces
            .get("org.bluez.Battery1")
            .and_then(|b| b.get("Percentage"))
            .and_then(|v| u8::try_from(v).ok())
            .map(|b| b.min(100));
        if battery.is_none() && kind_label == "手写笔" {
            battery = pen_docked_level;
        }

        let item = BtDeviceItem {
            path: path.to_string(),
            mac: mac.clone(),
            name: name.clone(),
            kind_label,
            paired: is_paired,
            connected: is_connected,
            battery,
        };

        if is_paired {
            paired.push(item);
        } else if !raw_name.is_empty()
            && !looks_like_raw_mac(&name)
            && !name.eq_ignore_ascii_case(&mac)
        {
            discovered.push(item);
        }
    }

    paired.sort_by(|a, b| {
        b.connected
            .cmp(&a.connected)
            .then_with(|| a.name.cmp(&b.name))
    });
    discovered.sort_by(|a, b| a.name.cmp(&b.name));

    BtState {
        enabled,
        discovering,
        paired,
        discovered,
    }
}

pub async fn set_bt_enabled(sys_bus: &zbus::Connection, enabled: bool) {
    let _ = sys_bus
        .call_method(
            Some("org.bluez"),
            "/org/bluez/hci0",
            Some("org.freedesktop.DBus.Properties"),
            "Set",
            &("org.bluez.Adapter1", "Powered", Value::from(enabled)),
        )
        .await;
}

pub async fn set_bt_discovery(sys_bus: &zbus::Connection, start: bool) {
    let method = if start {
        "StartDiscovery"
    } else {
        "StopDiscovery"
    };
    let _ = sys_bus
        .call_method(
            Some("org.bluez"),
            "/org/bluez/hci0",
            Some("org.bluez.Adapter1"),
            method,
            &(),
        )
        .await;
}

pub async fn toggle_bt_connection(sys_bus: &zbus::Connection, dev_path: &str, currently_connected: bool) {
    let method = if currently_connected {
        "Disconnect"
    } else {
        "Connect"
    };
    let _ = sys_bus
        .call_method(
            Some("org.bluez"),
            dev_path,
            Some("org.bluez.Device1"),
            method,
            &(),
        )
        .await;
}

pub async fn pair_and_connect_bt(sys_bus: &zbus::Connection, dev_path: &str) -> Result<(), String> {
    sys_bus
        .call_method(
            Some("org.bluez"),
            dev_path,
            Some("org.bluez.Device1"),
            "Pair",
            &(),
        )
        .await
        .map_err(|e| format!("配对失败: {e}"))?;

    let _ = sys_bus
        .call_method(
            Some("org.bluez"),
            dev_path,
            Some("org.freedesktop.DBus.Properties"),
            "Set",
            &("org.bluez.Device1", "Trusted", Value::from(true)),
        )
        .await;

    let _ = sys_bus
        .call_method(
            Some("org.bluez"),
            dev_path,
            Some("org.bluez.Device1"),
            "Connect",
            &(),
        )
        .await;
    Ok(())
}

pub async fn forget_bt_device(sys_bus: &zbus::Connection, dev_path: &str) {
    let Ok(obj_path) = ObjectPath::try_from(dev_path) else {
        return;
    };
    let _ = sys_bus
        .call_method(
            Some("org.bluez"),
            "/org/bluez/hci0",
            Some("org.bluez.Adapter1"),
            "RemoveDevice",
            &(obj_path,),
        )
        .await;
}

/// `org.bluez.Agent1` implementation (`DisplayYesNo`) so pairing headphones/keyboards
/// displays a 6-digit passkey confirmation dialog directly inside `tb520fu-settings`.
pub struct BluezAgentIface {
    sys_bus: zbus::Connection,
    prompt_tx: mpsc::UnboundedSender<PairingPrompt>,
    pending_cancel: Arc<Mutex<Option<oneshot::Sender<bool>>>>,
}

impl BluezAgentIface {
    pub fn new(
        sys_bus: zbus::Connection,
        prompt_tx: mpsc::UnboundedSender<PairingPrompt>,
    ) -> Self {
        Self {
            sys_bus,
            prompt_tx,
            pending_cancel: Arc::new(Mutex::new(None)),
        }
    }

    async fn resolve_device_name(&self, dev_path: &str) -> String {
        self.sys_bus
            .call_method(
                Some("org.bluez"),
                dev_path,
                Some("org.freedesktop.DBus.Properties"),
                "Get",
                &("org.bluez.Device1", "Alias"),
            )
            .await
            .ok()
            .and_then(|r| r.body().deserialize::<OwnedValue>().ok())
            .and_then(|v| String::try_from(v).ok())
            .unwrap_or_else(|| "蓝牙设备".into())
    }
}

#[zbus::interface(name = "org.bluez.Agent1")]
impl BluezAgentIface {
    #[zbus(name = "Release")]
    async fn release(&self) {}

    #[zbus(name = "RequestConfirmation")]
    async fn request_confirmation(
        &self,
        device: OwnedObjectPath,
        passkey: u32,
    ) -> zbus::fdo::Result<()> {
        let name = self.resolve_device_name(device.as_str()).await;
        let (tx, rx) = oneshot::channel();
        let _ = self.prompt_tx.send(PairingPrompt {
            device_path: device.to_string(),
            device_name: name,
            passkey: format!("{passkey:06}"),
            responder: tx,
        });
        match rx.await {
            Ok(true) => Ok(()),
            _ => Err(zbus::fdo::Error::Failed("Rejected".into())),
        }
    }

    #[zbus(name = "RequestAuthorization")]
    async fn request_authorization(&self, device: OwnedObjectPath) -> zbus::fdo::Result<()> {
        let name = self.resolve_device_name(device.as_str()).await;
        let (tx, rx) = oneshot::channel();
        let _ = self.prompt_tx.send(PairingPrompt {
            device_path: device.to_string(),
            device_name: name,
            passkey: "授权连接".into(),
            responder: tx,
        });
        match rx.await {
            Ok(true) => Ok(()),
            _ => Err(zbus::fdo::Error::Failed("Rejected".into())),
        }
    }

    #[zbus(name = "AuthorizeService")]
    async fn authorize_service(
        &self,
        _device: OwnedObjectPath,
        _uuid: String,
    ) -> zbus::fdo::Result<()> {
        Ok(())
    }

    #[zbus(name = "DisplayPasskey")]
    async fn display_passkey(&self, _device: OwnedObjectPath, _passkey: u32, _entered: u16) {}

    #[zbus(name = "Cancel")]
    async fn cancel(&self) {
        if let Some(tx) = self.pending_cancel.lock().await.take() {
            let _ = tx.send(false);
        }
    }
}

pub async fn register_pairing_agent(
    sys_bus: &zbus::Connection,
    prompt_tx: mpsc::UnboundedSender<PairingPrompt>,
) -> bool {
    let iface = BluezAgentIface::new(sys_bus.clone(), prompt_tx);
    let _ = sys_bus.object_server().at(AGENT_PATH, iface).await;
    let Ok(path) = ObjectPath::try_from(AGENT_PATH) else {
        return false;
    };
    let ok = sys_bus
        .call_method(
            Some("org.bluez"),
            "/org/bluez",
            Some("org.bluez.AgentManager1"),
            "RegisterAgent",
            &(path.clone(), "DisplayYesNo"),
        )
        .await
        .is_ok();
    if ok {
        let _ = sys_bus
            .call_method(
                Some("org.bluez"),
                "/org/bluez",
                Some("org.bluez.AgentManager1"),
                "RequestDefaultAgent",
                &(path,),
            )
            .await;
    }
    ok
}

pub async fn unregister_pairing_agent(sys_bus: &zbus::Connection) {
    if let Ok(path) = ObjectPath::try_from(AGENT_PATH) {
        let _ = sys_bus
            .call_method(
                Some("org.bluez"),
                "/org/bluez",
                Some("org.bluez.AgentManager1"),
                "UnregisterAgent",
                &(path,),
            )
            .await;
    }
}

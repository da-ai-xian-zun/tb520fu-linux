// SPDX-License-Identifier: MIT
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioNode {
    pub id: u32,
    pub name: String,
    pub is_default: bool,
}

#[derive(Debug, Clone)]
pub struct AudioState {
    pub out_vol_pct: u8,
    pub out_muted: bool,
    pub mic_vol_pct: u8,
    pub mic_muted: bool,
    pub sinks: Vec<AudioNode>,
    pub sources: Vec<AudioNode>,
}

impl Default for AudioState {
    fn default() -> Self {
        Self {
            out_vol_pct: 55,
            out_muted: false,
            mic_vol_pct: 80,
            mic_muted: false,
            sinks: Vec::new(),
            sources: Vec::new(),
        }
    }
}

pub fn parse_wpctl_volume(raw: &str) -> Option<(u8, bool)> {
    // Example: "Volume: 0.60" or "Volume: 0.45 [MUTED]"
    let muted = raw.contains("[MUTED]");
    let val_str = raw
        .split_whitespace()
        .nth(1)?;
    let v: f32 = val_str.parse().ok()?;
    let pct = (v * 100.0).round().clamp(0.0, 100.0) as u8;
    Some((pct, muted))
}

/// Parses `wpctl status` strictly inside the `Audio -> Sinks` and `Audio -> Sources` trees,
/// stopping before `Filters` / `Streams` / `Video` so internal speaker-protection filter nodes
/// (`tb520fu.speaker-protect`) are never exposed as selectable user outputs.
pub fn parse_wpctl_status_nodes(raw: &str) -> (Vec<AudioNode>, Vec<AudioNode>) {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Sec {
        None,
        Sinks,
        Sources,
    }

    let mut in_audio = false;
    let mut sec = Sec::None;
    let mut sinks = Vec::new();
    let mut sources = Vec::new();

    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed == "Audio" {
            in_audio = true;
            sec = Sec::None;
            continue;
        }
        if trimmed == "Video" || trimmed == "Settings" {
            in_audio = false;
            sec = Sec::None;
            continue;
        }
        if !in_audio {
            continue;
        }
        if trimmed.contains("Sinks:") {
            sec = Sec::Sinks;
            continue;
        }
        if trimmed.contains("Sources:") {
            sec = Sec::Sources;
            continue;
        }
        if trimmed.contains("Filters:")
            || trimmed.contains("Streams:")
            || trimmed.contains("Devices:")
            || trimmed.contains(" Sink endpoints:")
        {
            sec = Sec::None;
            continue;
        }
        if sec == Sec::None {
            continue;
        }

        // Strip box-drawing prefixes: "│  *   54. Built-in Audio Speakers             [vol: 0.60]"
        let body = line
            .trim_start_matches(|c: char| {
                c.is_whitespace() || matches!(c, '│' | '├' | '└' | '─')
            })
            .trim();
        if body.is_empty() {
            continue;
        }
        let (is_default, rest) = if let Some(r) = body.strip_prefix('*') {
            (true, r.trim_start())
        } else {
            (false, body)
        };
        let Some((id_part, after_dot)) = rest.split_once('.') else {
            continue;
        };
        let Ok(id) = id_part.trim().parse::<u32>() else {
            continue;
        };
        let name_part = match after_dot.rfind('[') {
            Some(idx) => &after_dot[..idx],
            None => after_dot,
        };
        let name = name_part.trim().to_string();
        if name.is_empty() || name.contains("speaker-protect") {
            continue;
        }
        let node = AudioNode {
            id,
            name,
            is_default,
        };
        match sec {
            Sec::Sinks => sinks.push(node),
            Sec::Sources => sources.push(node),
            Sec::None => {}
        }
    }

    (sinks, sources)
}

fn read_wpctl_vol(target: &str) -> Option<(u8, bool)> {
    let out = Command::new("wpctl")
        .args(["get-volume", target])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_wpctl_volume(&String::from_utf8_lossy(&out.stdout))
}

pub fn get_audio_state() -> AudioState {
    let (out_vol_pct, out_muted) = read_wpctl_vol("@DEFAULT_AUDIO_SINK@").unwrap_or((55, false));
    let (mic_vol_pct, mic_muted) = read_wpctl_vol("@DEFAULT_AUDIO_SOURCE@").unwrap_or((80, false));
    let (sinks, sources) = Command::new("wpctl")
        .arg("status")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| parse_wpctl_status_nodes(&String::from_utf8_lossy(&o.stdout)))
        .unwrap_or_default();

    AudioState {
        out_vol_pct,
        out_muted,
        mic_vol_pct,
        mic_muted,
        sinks,
        sources,
    }
}

pub fn set_output_volume(pct: u8) {
    let p = pct.min(100);
    let _ = Command::new("wpctl")
        .args(["set-volume", "@DEFAULT_AUDIO_SINK@", &format!("{p}%")])
        .status();
    if p > 0 {
        let _ = Command::new("wpctl")
            .args(["set-mute", "@DEFAULT_AUDIO_SINK@", "0"])
            .status();
    }
}

pub fn set_output_mute(muted: bool) {
    let _ = Command::new("wpctl")
        .args([
            "set-mute",
            "@DEFAULT_AUDIO_SINK@",
            if muted { "1" } else { "0" },
        ])
        .status();
}

pub fn set_mic_volume(pct: u8) {
    let p = pct.min(100);
    let _ = Command::new("wpctl")
        .args(["set-volume", "@DEFAULT_AUDIO_SOURCE@", &format!("{p}%")])
        .status();
}

pub fn set_mic_mute(muted: bool) {
    let _ = Command::new("wpctl")
        .args([
            "set-mute",
            "@DEFAULT_AUDIO_SOURCE@",
            if muted { "1" } else { "0" },
        ])
        .status();
}

pub fn set_default_node(id: u32) {
    let _ = Command::new("wpctl")
        .args(["set-default", &id.to_string()])
        .status();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_wpctl_sinks_and_sources_while_ignoring_speaker_protect_filter() {
        let sample = r#"
Audio
 ├─ Devices:
 │      50. Built-in Audio                      [alsa]
 │
 ├─ Sinks:
 │  *   54. Built-in Audio Speakers             [vol: 0.60]
 │      79. INZONE Buds                         [vol: 0.45]
 │
 ├─ Sources:
 │  *   55. Built-in Audio Internal Microphones [vol: 1.00]
 │
 ├─ Filters:
 │    - filter-chain-950-8
 │      42. tb520fu.speaker-protect                                      [Audio/Sink]
 └─ Streams:
Video
"#;
        let (sinks, sources) = parse_wpctl_status_nodes(sample);
        assert_eq!(sinks.len(), 2);
        assert_eq!(
            sinks[0],
            AudioNode {
                id: 54,
                name: "Built-in Audio Speakers".into(),
                is_default: true
            }
        );
        assert_eq!(
            sinks[1],
            AudioNode {
                id: 79,
                name: "INZONE Buds".into(),
                is_default: false
            }
        );
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].id, 55);
        assert_eq!(parse_wpctl_volume("Volume: 0.60 [MUTED]\n"), Some((60, true)));
    }
}

// SPDX-License-Identifier: MIT
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppRole {
    Browser,
    Pdf,
    Image,
    Media,
}

impl AppRole {
    pub fn primary_mime(self) -> &'static str {
        match self {
            Self::Browser => "x-scheme-handler/https",
            Self::Pdf => "application/pdf",
            Self::Image => "image/png",
            Self::Media => "video/mp4",
        }
    }

    pub fn synced_mimes(self) -> &'static [&'static str] {
        match self {
            Self::Browser => &[
                "x-scheme-handler/http",
                "x-scheme-handler/https",
                "text/html",
                "application/xhtml+xml",
            ],
            Self::Pdf => &["application/pdf"],
            Self::Image => &[
                "image/png",
                "image/jpeg",
                "image/webp",
                "image/gif",
                "image/bmp",
            ],
            Self::Media => &[
                "video/mp4",
                "video/x-matroska",
                "video/webm",
                "audio/mpeg",
                "audio/flac",
                "audio/ogg",
            ],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopAppChoice {
    pub desktop_id: String,
    pub name: String,
}

#[derive(Debug, Clone, Default)]
pub struct DefaultAppsSnapshot {
    pub browsers: Vec<DesktopAppChoice>,
    pub browser_idx: usize,
    pub browser_label: String,

    pub pdf_apps: Vec<DesktopAppChoice>,
    pub pdf_idx: usize,
    pub pdf_label: String,

    pub image_apps: Vec<DesktopAppChoice>,
    pub image_idx: usize,
    pub image_label: String,

    pub media_apps: Vec<DesktopAppChoice>,
    pub media_idx: usize,
    pub media_label: String,
}

#[derive(Debug, Clone)]
struct ParsedDesktop {
    desktop_id: String,
    name: String,
    mimes: Vec<String>,
    no_display: bool,
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/home/debian"))
}

fn mimeapps_path() -> PathBuf {
    home_dir().join(".config/mimeapps.list")
}

fn app_search_dirs() -> Vec<PathBuf> {
    let home = home_dir();
    vec![
        home.join(".local/share/applications"),
        PathBuf::from("/var/lib/flatpak/exports/share/applications"),
        PathBuf::from("/usr/local/share/applications"),
        PathBuf::from("/usr/share/applications"),
    ]
}

fn parse_desktop_content(desktop_id: &str, raw: &str) -> Option<ParsedDesktop> {
    let mut in_entry = false;
    let mut is_app = false;
    let mut hidden = false;
    let mut no_display = false;
    let mut name_en = String::new();
    let mut name_zh = String::new();
    let mut mimes = Vec::new();

    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_entry = trimmed == "[Desktop Entry]";
            continue;
        }
        if !in_entry {
            continue;
        }
        let Some((k, v)) = trimmed.split_once('=') else {
            continue;
        };
        let key = k.trim();
        let val = v.trim();
        match key {
            "Type" => is_app = val == "Application",
            "Hidden" => hidden = val.eq_ignore_ascii_case("true"),
            "NoDisplay" => no_display = val.eq_ignore_ascii_case("true"),
            "Name" if name_en.is_empty() => name_en = val.to_string(),
            "Name[zh_CN]" | "Name[zh]" if name_zh.is_empty() => name_zh = val.to_string(),
            "MimeType" => {
                mimes = val
                    .split(';')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_ascii_lowercase)
                    .collect();
            }
            _ => {}
        }
    }

    if !is_app || hidden {
        return None;
    }
    let name = if !name_zh.is_empty() {
        name_zh
    } else if !name_en.is_empty() {
        name_en
    } else {
        desktop_id.trim_end_matches(".desktop").to_string()
    };

    Some(ParsedDesktop {
        desktop_id: desktop_id.to_string(),
        name,
        mimes,
        no_display,
    })
}

fn scan_installed_desktops_in(dirs: &[PathBuf]) -> Vec<ParsedDesktop> {
    let mut seen = HashMap::<String, ParsedDesktop>::new();
    let mut order = Vec::<String>::new();

    for dir in dirs {
        let Ok(entries) = fs::read_dir(dir) else {
            continue;
        };
        let mut files: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        files.sort();
        for p in files {
            if p.extension().and_then(|e| e.to_str()) != Some("desktop") {
                continue;
            }
            let Some(fname) = p.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if seen.contains_key(fname) {
                continue;
            }
            let Ok(raw) = fs::read_to_string(&p) else {
                continue;
            };
            if let Some(parsed) = parse_desktop_content(fname, &raw) {
                order.push(fname.to_string());
                seen.insert(fname.to_string(), parsed);
            }
        }
    }

    order
        .into_iter()
        .filter_map(|k| seen.remove(&k))
        .collect()
}

pub fn parse_mimeapps_defaults(raw: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let mut in_defaults = false;
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_defaults = trimmed == "[Default Applications]";
            continue;
        }
        if !in_defaults {
            continue;
        }
        if let Some((mime, apps)) = trimmed.split_once('=') {
            if let Some(first) = apps
                .split(';')
                .map(str::trim)
                .find(|s| !s.is_empty())
            {
                map.insert(mime.trim().to_ascii_lowercase(), first.to_string());
            }
        }
    }
    map
}

pub fn update_mimeapps_text(raw: &str, mimes: &[&str], desktop_id: &str) -> String {
    let targets: HashMap<String, &str> = mimes
        .iter()
        .map(|m| (m.to_ascii_lowercase(), desktop_id))
        .collect();

    let mut out_lines: Vec<String> = Vec::new();
    let mut found_section = false;
    let mut in_defaults = false;
    let mut written_keys = Vec::<String>::new();

    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            if in_defaults {
                for &m in mimes {
                    let ml = m.to_ascii_lowercase();
                    if !written_keys.contains(&ml) {
                        out_lines.push(format!("{m}={desktop_id}"));
                        written_keys.push(ml);
                    }
                }
            }
            in_defaults = trimmed == "[Default Applications]";
            if in_defaults {
                found_section = true;
            }
            out_lines.push(line.to_string());
            continue;
        }

        if in_defaults {
            if let Some((k, _)) = trimmed.split_once('=') {
                let kl = k.trim().to_ascii_lowercase();
                if let Some(&desk) = targets.get(&kl) {
                    if !written_keys.contains(&kl) {
                        out_lines.push(format!("{}={desk}", k.trim()));
                        written_keys.push(kl);
                    }
                    continue;
                }
            }
        }
        out_lines.push(line.to_string());
    }

    if !found_section {
        if !out_lines.is_empty() && !out_lines.last().unwrap().is_empty() {
            out_lines.push(String::new());
        }
        out_lines.push("[Default Applications]".into());
        for &m in mimes {
            out_lines.push(format!("{m}={desktop_id}"));
        }
    } else if in_defaults {
        for &m in mimes {
            let ml = m.to_ascii_lowercase();
            if !written_keys.contains(&ml) {
                out_lines.push(format!("{m}={desktop_id}"));
                written_keys.push(ml);
            }
        }
    }

    let mut joined = out_lines.join("\n");
    if !joined.ends_with('\n') {
        joined.push('\n');
    }
    joined
}

fn build_role_choices(
    apps: &[ParsedDesktop],
    defaults: &HashMap<String, String>,
    role: AppRole,
) -> (Vec<DesktopAppChoice>, usize, String) {
    let wanted = role.synced_mimes();
    let cur_id = defaults
        .get(role.primary_mime())
        .or_else(|| wanted.iter().find_map(|m| defaults.get(*m)))
        .cloned();

    let mut choices: Vec<DesktopAppChoice> = apps
        .iter()
        .filter(|a| {
            let matches_mime = a
                .mimes
                .iter()
                .any(|am| wanted.iter().any(|wm| am == wm));
            matches_mime && (!a.no_display || cur_id.as_deref() == Some(a.desktop_id.as_str()))
        })
        .map(|a| DesktopAppChoice {
            desktop_id: a.desktop_id.clone(),
            name: a.name.clone(),
        })
        .collect();

    choices.sort_by(|a, b| a.name.cmp(&b.name));
    choices.dedup_by(|a, b| a.desktop_id == b.desktop_id);

    if choices.is_empty() {
        return (Vec::new(), 0, "未安装对应应用".into());
    }

    let preferred: &[&str] = match role {
        AppRole::Browser => &["firefox-esr.desktop", "firefox.desktop", "chromium.desktop"],
        AppRole::Pdf => &["org.gnome.Papers.desktop", "org.gnome.Evince.desktop"],
        AppRole::Image => &["org.gnome.Loupe.desktop", "org.gnome.eog.desktop"],
        AppRole::Media => &["mpv.desktop", "io.bassi.Amberol.desktop"],
    };

    let idx = cur_id
        .as_deref()
        .and_then(|cid| choices.iter().position(|c| c.desktop_id == cid))
        .or_else(|| {
            preferred
                .iter()
                .find_map(|pid| choices.iter().position(|c| c.desktop_id == *pid))
        })
        .unwrap_or(0);
    let label = choices[idx].name.clone();
    (choices, idx, label)
}

pub fn query_default_apps() -> DefaultAppsSnapshot {
    let apps = scan_installed_desktops_in(&app_search_dirs());
    let raw_mime = fs::read_to_string(mimeapps_path()).unwrap_or_default();
    let defaults = parse_mimeapps_defaults(&raw_mime);

    let (browsers, browser_idx, browser_label) = build_role_choices(&apps, &defaults, AppRole::Browser);
    let (pdf_apps, pdf_idx, pdf_label) = build_role_choices(&apps, &defaults, AppRole::Pdf);
    let (image_apps, image_idx, image_label) = build_role_choices(&apps, &defaults, AppRole::Image);
    let (media_apps, media_idx, media_label) = build_role_choices(&apps, &defaults, AppRole::Media);

    DefaultAppsSnapshot {
        browsers,
        browser_idx,
        browser_label,
        pdf_apps,
        pdf_idx,
        pdf_label,
        image_apps,
        image_idx,
        image_label,
        media_apps,
        media_idx,
        media_label,
    }
}

pub fn set_default_app(role: AppRole, desktop_id: &str) -> Result<(), String> {
    let path = mimeapps_path();
    set_default_app_at_path(&path, role, desktop_id)
}

pub fn set_default_app_at_path(
    path: &Path,
    role: AppRole,
    desktop_id: &str,
) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let existing = fs::read_to_string(path).unwrap_or_default();
    let updated = update_mimeapps_text(&existing, role.synced_mimes(), desktop_id);
    let tmp = path.with_extension("list.tmp");
    fs::write(&tmp, updated).map_err(|e| e.to_string())?;
    fs::rename(&tmp, path).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_desktop_and_preserves_other_mimeapps_sections() {
        let sample_desktop = "\
[Desktop Entry]
Type=Application
Name=Papers
Name[zh_CN]=文档查看器 (Papers)
MimeType=application/pdf;image/vnd.djvu;
";
        let parsed = parse_desktop_content("org.gnome.Papers.desktop", sample_desktop).unwrap();
        assert_eq!(parsed.name, "文档查看器 (Papers)");
        assert!(parsed.mimes.contains(&"application/pdf".to_string()));

        let initial_mimeapps = "\
[Default Applications]
text/plain=nvim.desktop
application/pdf=evince.desktop

[Added Associations]
video/mp4=mpv.desktop;
";
        let next = update_mimeapps_text(
            initial_mimeapps,
            AppRole::Pdf.synced_mimes(),
            "org.gnome.Papers.desktop",
        );
        let parsed_map = parse_mimeapps_defaults(&next);
        assert_eq!(
            parsed_map.get("application/pdf").map(String::as_str),
            Some("org.gnome.Papers.desktop")
        );
        assert_eq!(
            parsed_map.get("text/plain").map(String::as_str),
            Some("nvim.desktop")
        );
        assert!(next.contains("[Added Associations]\nvideo/mp4=mpv.desktop;"));
    }
}

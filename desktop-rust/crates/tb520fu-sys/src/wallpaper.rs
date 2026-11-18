// SPDX-License-Identifier: MIT
use std::fs;
use std::path::{Path, PathBuf};

pub const WALLPAPER_PRESETS: [(&str, &str); 5] = [
    ("stock", "ZUI 原厂流羽"),
    ("plume", "流羽暖金"),
    ("aurora", "极光青翠"),
    ("twilight", "暮霞星云"),
    ("mist", "云山素雪"),
];

#[derive(Debug, Clone)]
pub struct LocalWallpaperItem {
    pub label: String,
    pub spec: String,
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/home/debian"))
}

pub fn preset_index(spec: &str) -> Option<usize> {
    WALLPAPER_PRESETS
        .iter()
        .position(|(id, _)| *id == spec.trim())
}

pub fn describe_wallpaper_spec(spec: &str) -> String {
    let s = spec.trim();
    if let Some((_, label)) = WALLPAPER_PRESETS.iter().find(|(id, _)| *id == s) {
        return (*label).to_string();
    }
    if let Some(p) = s
        .strip_prefix("custom:")
        .or_else(|| s.starts_with('/').then_some(s))
    {
        let fname = Path::new(p)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| p.to_string());
        return format!("本地：{fname}");
    }
    "ZUI 原厂流羽".into()
}

pub fn scan_local_wallpapers() -> Vec<LocalWallpaperItem> {
    let home = home_dir();
    let dirs = [
        home.join("Pictures/Wallpapers"),
        home.join("Pictures"),
        home.join("Downloads"),
        PathBuf::from("/usr/share/tb520fu"),
        PathBuf::from("/usr/share/backgrounds"),
    ];
    let mut out = Vec::new();
    let mut seen = Vec::new();

    for dir in &dirs {
        collect_images_in_dir(dir, 1, &mut seen, &mut out);
        if out.len() >= 24 {
            break;
        }
    }
    out
}

fn collect_images_in_dir(
    dir: &Path,
    depth_left: u8,
    seen: &mut Vec<PathBuf>,
    out: &mut Vec<LocalWallpaperItem>,
) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();

    for p in paths {
        if out.len() >= 24 {
            break;
        }
        if p.is_dir() && depth_left > 0 {
            collect_images_in_dir(&p, depth_left - 1, seen, out);
            continue;
        }
        if !p.is_file() {
            continue;
        }
        let ext_ok = p
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|ext| matches!(ext.to_ascii_lowercase().as_str(), "jpg" | "jpeg" | "png"));
        if !ext_ok {
            continue;
        }
        let canon = fs::canonicalize(&p).unwrap_or_else(|_| p.clone());
        if seen.contains(&canon) {
            continue;
        }
        seen.push(canon.clone());
        let fname = p
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| p.display().to_string());
        let parent_hint = p
            .parent()
            .and_then(|par| par.file_name())
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let label = if parent_hint.is_empty() {
            fname
        } else {
            format!("{fname} ({parent_hint})")
        };
        out.push(LocalWallpaperItem {
            label,
            spec: format!("custom:{}", canon.display()),
        });
    }
}

pub fn validate_custom_wallpaper_path(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim().trim_matches('"').trim_matches('\'');
    if trimmed.is_empty() {
        return Err("请输入图片文件路径".into());
    }
    let stripped = trimmed.strip_prefix("custom:").unwrap_or(trimmed);
    let expanded = if let Some(rest) = stripped.strip_prefix("~/") {
        home_dir().join(rest)
    } else {
        PathBuf::from(stripped)
    };
    if !expanded.is_file() {
        return Err(format!("未找到图片文件：{}", expanded.display()));
    }
    let ext_ok = expanded
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| matches!(ext.to_ascii_lowercase().as_str(), "jpg" | "jpeg" | "png"));
    if !ext_ok {
        return Err("目前仅支持 PNG 或 JPG/JPEG 格式壁纸".into());
    }
    let canon = fs::canonicalize(&expanded).unwrap_or(expanded);
    Ok(format!("custom:{}", canon.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preset_lookup_and_custom_description_format_cleanly() {
        assert_eq!(preset_index("aurora"), Some(2));
        assert_eq!(describe_wallpaper_spec("twilight"), "暮霞星云");
        assert_eq!(
            describe_wallpaper_spec("custom:/home/debian/Pictures/ocean.png"),
            "本地：ocean.png"
        );
    }
}

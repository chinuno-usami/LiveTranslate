//! 多配置档：`config.toml` 始终是权威文件，`profiles/<name>.toml` 是命名快照。
//!
//! 配置档只在「使用用户配置目录」时启用；`--config` 或便携模式下禁用，
//! 避免与显式指定的配置语义冲突。

use crate::config::writer;
use crate::config::AppConfig;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

pub const DEFAULT_PROFILE: &str = "default";

#[derive(Debug, Clone, Serialize)]
pub struct ProfileEntry {
    pub name: String,
    pub path: String,
    pub updated_at: u64,
    pub valid: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProfilesStatus {
    pub enabled: bool,
    pub reason: Option<String>,
    pub active: Option<String>,
    pub profiles: Vec<ProfileEntry>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ProfilesMeta {
    #[serde(default)]
    active: Option<String>,
}

/// 判断当前配置路径下是否允许使用配置档
pub fn profiles_enabled(config_path: Option<&Path>) -> (bool, Option<String>) {
    let Some(path) = config_path else {
        return (
            false,
            Some("当前使用内置默认配置，保存一次设置后即可使用配置档".to_string()),
        );
    };
    let user_path = match AppConfig::user_config_path() {
        Ok(p) => p,
        Err(e) => return (false, Some(format!("无法确定用户配置目录: {e}"))),
    };
    if same_path(path, &user_path) {
        (true, None)
    } else {
        (
            false,
            Some(format!(
                "当前使用显式指定的配置文件（{}），配置档已禁用",
                path.display()
            )),
        )
    }
}

fn same_path(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

pub fn profiles_dir(config_path: &Path) -> PathBuf {
    config_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("profiles")
}

fn meta_path(config_path: &Path) -> PathBuf {
    config_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("profiles.toml")
}

fn profile_path(config_path: &Path, name: &str) -> PathBuf {
    profiles_dir(config_path).join(format!("{name}.toml"))
}

/// 首次启用时做非破坏性迁移：创建 profiles/ 与 default 快照
pub fn ensure_initialized(config_path: &Path) -> Result<(), String> {
    let dir = profiles_dir(config_path);
    fs::create_dir_all(&dir).map_err(|e| format!("创建配置档目录失败: {e}"))?;

    let default_path = dir.join(format!("{DEFAULT_PROFILE}.toml"));
    if !default_path.exists() {
        let text = match fs::read_to_string(config_path) {
            Ok(text) => text,
            Err(_) => AppConfig::default()
                .to_toml_string()
                .map_err(|e| format!("生成默认配置失败: {e}"))?,
        };
        writer::write_atomic(&default_path, &text)?;
    }

    let meta = meta_path(config_path);
    if !meta.exists() {
        let active = read_active(config_path).unwrap_or_else(|| DEFAULT_PROFILE.to_string());
        write_active(config_path, &active)?;
    }
    Ok(())
}

fn read_meta(config_path: &Path) -> Option<ProfilesMeta> {
    let text = fs::read_to_string(meta_path(config_path)).ok()?;
    toml::from_str(&text).ok()
}

pub fn read_active(config_path: &Path) -> Option<String> {
    read_meta(config_path).and_then(|m| m.active)
}

pub fn write_active(config_path: &Path, name: &str) -> Result<(), String> {
    let meta = ProfilesMeta {
        active: Some(name.to_string()),
    };
    let text = toml::to_string(&meta).map_err(|e| format!("序列化 profiles.toml 失败: {e}"))?;
    writer::write_atomic(&meta_path(config_path), &text)
}

/// 校验配置档名（跨平台安全）
pub fn validate_name(name: &str) -> Result<(), String> {
    let chars: Vec<char> = name.chars().collect();
    if chars.is_empty() || chars.len() > 40 {
        return Err("配置档名必须为 1–40 个字符".to_string());
    }
    if name.trim() != name {
        return Err("配置档名不能以空白开头或结尾".to_string());
    }
    if name.starts_with('.') || name.ends_with('.') {
        return Err("配置档名不能以点开头或结尾".to_string());
    }
    if !chars
        .iter()
        .all(|c| c.is_alphanumeric() || *c == '_' || *c == '-')
    {
        return Err("配置档名只能包含字母、数字、中文、下划线和短横线".to_string());
    }
    let upper = name.to_ascii_uppercase();
    let reserved = ["CON", "PRN", "AUX", "NUL"];
    if reserved.contains(&upper.as_str())
        || (upper.len() == 4
            && (upper.starts_with("COM") || upper.starts_with("LPT"))
            && upper.as_bytes()[3].is_ascii_digit())
    {
        return Err(format!("「{name}」是 Windows 保留名，无法作为配置档名"));
    }
    Ok(())
}

pub fn list_status(config_path: Option<&Path>) -> ProfilesStatus {
    let (enabled, reason) = profiles_enabled(config_path);
    let Some(config_path) = config_path else {
        return ProfilesStatus {
            enabled,
            reason,
            active: None,
            profiles: Vec::new(),
        };
    };
    if !enabled {
        return ProfilesStatus {
            enabled,
            reason,
            active: read_active(config_path),
            profiles: Vec::new(),
        };
    }
    if let Err(e) = ensure_initialized(config_path) {
        return ProfilesStatus {
            enabled: false,
            reason: Some(e),
            active: None,
            profiles: Vec::new(),
        };
    }

    let active = read_active(config_path);
    let dir = profiles_dir(config_path);
    let mut profiles = Vec::new();

    if let Ok(entries) = fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                continue;
            }
            let Some(name) = path
                .file_stem()
                .and_then(|s| s.to_str())
                .map(str::to_string)
            else {
                continue;
            };
            let (valid, error) = match fs::read_to_string(&path) {
                Ok(text) => match AppConfig::from_str(&text).and_then(|cfg| {
                    cfg.validate().map_err(crate::error::AppError::Config)?;
                    Ok(cfg)
                }) {
                    Ok(_) => (true, None),
                    Err(e) => (false, Some(e.to_string())),
                },
                Err(e) => (false, Some(e.to_string())),
            };
            let updated_at = entry
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            profiles.push(ProfileEntry {
                name,
                path: path.display().to_string(),
                updated_at,
                valid,
                error,
            });
        }
    }
    profiles.sort_by(|a, b| a.name.cmp(&b.name));

    ProfilesStatus {
        enabled: true,
        reason: None,
        active,
        profiles,
    }
}

fn validated_text(text: &str) -> Result<AppConfig, String> {
    let cfg = AppConfig::from_str(text).map_err(|e| format!("配置解析失败: {e}"))?;
    cfg.validate()?;
    Ok(cfg)
}

/// 新建配置档；`copy_current = true` 时从当前 config.toml 复制
pub fn create(config_path: &Path, name: &str, copy_from: Option<&Path>) -> Result<(), String> {
    validate_name(name)?;
    ensure_initialized(config_path)?;
    let target = profile_path(config_path, name);
    if target.exists() {
        return Err(format!("配置档「{name}」已存在"));
    }
    let text = match copy_from {
        Some(source) => fs::read_to_string(source).map_err(|e| format!("读取源配置失败: {e}"))?,
        None => AppConfig::default()
            .to_toml_string()
            .map_err(|e| format!("生成默认配置失败: {e}"))?,
    };
    validated_text(&text)?;
    writer::write_atomic(&target, &text)
}

pub fn duplicate(config_path: &Path, from: &str, to: &str) -> Result<(), String> {
    validate_name(from)?;
    validate_name(to)?;
    ensure_initialized(config_path)?;
    let source = profile_path(config_path, from);
    if !source.exists() {
        return Err(format!("配置档「{from}」不存在"));
    }
    create(config_path, to, Some(&source))
}

pub fn rename(config_path: &Path, old: &str, new: &str) -> Result<(), String> {
    validate_name(old)?;
    validate_name(new)?;
    ensure_initialized(config_path)?;
    let source = profile_path(config_path, old);
    let target = profile_path(config_path, new);
    if !source.exists() {
        return Err(format!("配置档「{old}」不存在"));
    }
    if target.exists() {
        return Err(format!("配置档「{new}」已存在"));
    }
    fs::rename(&source, &target).map_err(|e| format!("重命名失败: {e}"))?;
    if read_active(config_path).as_deref() == Some(old) {
        write_active(config_path, new)?;
    }
    Ok(())
}

pub fn delete(config_path: &Path, name: &str) -> Result<(), String> {
    validate_name(name)?;
    ensure_initialized(config_path)?;
    let active = read_active(config_path);
    if active.as_deref() == Some(name) {
        return Err("不能删除当前激活的配置档，请先切换到其它配置档".to_string());
    }
    let path = profile_path(config_path, name);
    if !path.exists() {
        return Err(format!("配置档「{name}」不存在"));
    }
    fs::remove_file(&path).map_err(|e| format!("删除失败: {e}"))
}

/// 校验目标配置档并返回其文本（实际替换由调用方在持有 lifecycle 锁时完成）
pub fn switch(config_path: &Path, name: &str) -> Result<String, String> {
    validate_name(name)?;
    ensure_initialized(config_path)?;
    let path = profile_path(config_path, name);
    if !path.exists() {
        return Err(format!("配置档「{name}」不存在"));
    }
    let text = fs::read_to_string(&path).map_err(|e| format!("读取配置档失败: {e}"))?;
    validated_text(&text)?;
    Ok(text)
}

/// 把当前配置镜像到激活的配置档（保存设置后调用）
pub fn mirror(config_path: &Path, active: &str, text: &str) -> Result<(), String> {
    ensure_initialized(config_path)?;
    validate_name(active)?;
    let path = profile_path(config_path, active);
    writer::write_atomic(&path, text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "livetranslate-profiles-{}-{}",
            std::process::id(),
            tag
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir.join("config.toml")
    }

    #[test]
    fn migration_creates_default_profile() {
        let config = setup("migrate");
        fs::write(&config, AppConfig::default().to_toml_string().unwrap()).unwrap();
        ensure_initialized(&config).unwrap();
        assert!(profiles_dir(&config).join("default.toml").exists());
        assert!(meta_path(&config).exists());
        assert_eq!(read_active(&config).as_deref(), Some("default"));
        let _ = fs::remove_dir_all(config.parent().unwrap());
    }

    #[test]
    fn create_duplicate_rename_delete() {
        let config = setup("crud");
        fs::write(&config, AppConfig::default().to_toml_string().unwrap()).unwrap();
        create(&config, "vr", Some(&config)).unwrap();
        assert!(profile_path(&config, "vr").exists());
        duplicate(&config, "vr", "stream").unwrap();
        assert!(profile_path(&config, "stream").exists());
        rename(&config, "stream", "直播").unwrap();
        assert!(profile_path(&config, "直播").exists());
        delete(&config, "直播").unwrap();
        assert!(!profile_path(&config, "直播").exists());
        let _ = fs::remove_dir_all(config.parent().unwrap());
    }

    #[test]
    fn cannot_delete_active_profile() {
        let config = setup("active");
        fs::write(&config, AppConfig::default().to_toml_string().unwrap()).unwrap();
        create(&config, "vr", None).unwrap();
        write_active(&config, "vr").unwrap();
        assert!(delete(&config, "vr").is_err());
        let _ = fs::remove_dir_all(config.parent().unwrap());
    }

    #[test]
    fn rejects_invalid_names() {
        for name in [
            "", " ", ".hidden", "a/b", "a:b", "con", "COM1", "a b", "名字.",
        ] {
            assert!(validate_name(name).is_err(), "{name} 应被拒绝");
        }
        for name in ["default", "vr", "直播", "my-setup_2"] {
            assert!(validate_name(name).is_ok(), "{name} 应被接受");
        }
    }

    #[test]
    fn switch_rejects_corrupt_profile() {
        let config = setup("corrupt");
        fs::write(&config, AppConfig::default().to_toml_string().unwrap()).unwrap();
        ensure_initialized(&config).unwrap();
        fs::write(profile_path(&config, "bad"), "not a config [").unwrap();
        assert!(switch(&config, "bad").is_err());
        let _ = fs::remove_dir_all(config.parent().unwrap());
    }

    #[test]
    fn disabled_with_explicit_config() {
        let (enabled, reason) = profiles_enabled(Some(Path::new("/tmp/some-other-config.toml")));
        assert!(!enabled);
        assert!(reason.is_some());
        assert!(!profiles_enabled(None).0);
    }
}

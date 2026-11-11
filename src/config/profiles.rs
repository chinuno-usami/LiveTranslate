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

/// 随包默认配置（带注释），用于新建配置档 / 首次迁移的快照
fn seed_text() -> String {
    AppConfig::SHIPPED_DEFAULT.to_string()
}

/// 首次启用时做非破坏性迁移：创建 profiles/ 与 default 快照
pub fn ensure_initialized(config_path: &Path) -> Result<(), String> {
    let _guard = writer::lock_writes();
    ensure_initialized_unlocked(config_path)
}

fn ensure_initialized_unlocked(config_path: &Path) -> Result<(), String> {
    let dir = profiles_dir(config_path);
    let default_path = dir.join(format!("{DEFAULT_PROFILE}.toml"));
    let pending_default = if !default_path.exists() {
        let text = match fs::read_to_string(config_path) {
            Ok(text) => {
                validated_text(&text).map_err(|e| {
                    format!("当前配置文件存在待修复项（{e}），请先保存修复后再操作配置档")
                })?;
                text
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => seed_text(),
            // 权限 / 编码错误不要退化成“文件不存在”，否则会静默生成无注释默认档
            Err(e) => return Err(format!("读取当前配置失败: {e}")),
        };
        Some(text)
    } else {
        None
    };

    fs::create_dir_all(&dir).map_err(|e| format!("创建配置档目录失败: {e}"))?;
    if let Some(text) = pending_default {
        writer::write_atomic_unlocked(&default_path, &text)?;
    }

    let meta = meta_path(config_path);
    if !meta.exists() || read_meta(config_path).is_none() {
        let active = read_active(config_path).unwrap_or_else(|| DEFAULT_PROFILE.to_string());
        write_active_unlocked(config_path, &active)?;
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

#[cfg(test)]
pub fn write_active(config_path: &Path, name: &str) -> Result<(), String> {
    let _guard = writer::lock_writes();
    write_active_unlocked(config_path, name)
}

fn write_active_unlocked(config_path: &Path, name: &str) -> Result<(), String> {
    let meta = ProfilesMeta {
        active: Some(name.to_string()),
    };
    let text = toml::to_string(&meta).map_err(|e| format!("序列化 profiles.toml 失败: {e}"))?;
    writer::write_atomic_unlocked(&meta_path(config_path), &text)
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
    if !chars.iter().all(|c| {
        (c.is_alphanumeric() && !matches!(c, '\u{00B2}' | '\u{00B3}' | '\u{00B9}'))
            || *c == '_'
            || *c == '-'
    }) {
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

/// 列出配置档状态；当 `allow_init` 为 `false`（当前配置处于损坏/待修复降级态）时，
/// 不主动把损坏的 `config.toml` 拷贝初始化成 `profiles/default.toml`。
pub fn list_status_with_init(config_path: Option<&Path>, allow_init: bool) -> ProfilesStatus {
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
    let _guard = writer::lock_writes();
    if allow_init {
        if let Err(e) = ensure_initialized_unlocked(config_path) {
            return ProfilesStatus {
                enabled: false,
                reason: Some(e),
                active: None,
                profiles: Vec::new(),
            };
        }
    } else if !meta_path(config_path).exists() || !profiles_dir(config_path).exists() {
        return ProfilesStatus {
            enabled: false,
            reason: Some("当前配置文件存在待修复项，保存修复后自动启用配置档".to_string()),
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
                    Ok(_) => match validate_name(&name) {
                        // 名字非法的手写文件：列出来但明确标记不可操作
                        Ok(()) => (true, None),
                        Err(e) => (false, Some(e)),
                    },
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

/// 新建配置档；`copy_from` 有值时从给定文件复制
pub fn create(config_path: &Path, name: &str, copy_from: Option<&Path>) -> Result<(), String> {
    validate_name(name)?;
    if name.eq_ignore_ascii_case(DEFAULT_PROFILE) && name != DEFAULT_PROFILE {
        return Err("配置档「default」已存在".to_string());
    }
    let _guard = writer::lock_writes();
    ensure_initialized_unlocked(config_path)?;
    let target = profile_path(config_path, name);
    if target.exists() {
        return Err(format!("配置档「{name}」已存在"));
    }
    let text = match copy_from {
        Some(source) => fs::read_to_string(source).map_err(|e| format!("读取源配置失败: {e}"))?,
        None => seed_text(),
    };
    validated_text(&text)?;
    writer::write_atomic_unlocked(&target, &text)
}

pub fn duplicate(config_path: &Path, from: &str, to: &str) -> Result<(), String> {
    validate_name(from)?;
    validate_name(to)?;
    if to.eq_ignore_ascii_case(DEFAULT_PROFILE) && to != DEFAULT_PROFILE {
        return Err("配置档「default」已存在".to_string());
    }
    let _guard = writer::lock_writes();
    ensure_initialized_unlocked(config_path)?;
    let source = profile_path(config_path, from);
    if !source.exists() {
        return Err(format!("配置档「{from}」不存在"));
    }
    let target = profile_path(config_path, to);
    if target.exists() {
        return Err(format!("配置档「{to}」已存在"));
    }
    let text = fs::read_to_string(&source).map_err(|e| format!("读取源配置失败: {e}"))?;
    validated_text(&text)?;
    writer::write_atomic_unlocked(&target, &text)
}

pub fn rename(config_path: &Path, old: &str, new: &str) -> Result<(), String> {
    validate_name(old)?;
    validate_name(new)?;
    if old.eq_ignore_ascii_case(DEFAULT_PROFILE) {
        return Err("不能重命名 default 配置档".to_string());
    }
    if old == new {
        return Ok(());
    }
    if new.eq_ignore_ascii_case(DEFAULT_PROFILE) {
        return Err("配置档「default」已存在".to_string());
    }
    let _guard = writer::lock_writes();
    ensure_initialized_unlocked(config_path)?;
    let source = profile_path(config_path, old);
    let target = profile_path(config_path, new);
    if !source.exists() {
        return Err(format!("配置档「{old}」不存在"));
    }
    let case_only = target.exists() && same_path(&source, &target);
    if target.exists() && !case_only {
        return Err(format!("配置档「{new}」已存在"));
    }
    if case_only {
        // 大小写不敏感文件系统（NTFS / 默认 APFS / FAT）经临时名两步改名，确保目录项大小写刷新
        let tmp =
            profiles_dir(config_path).join(format!(".rename-{}-{}.tmp", std::process::id(), old));
        fs::rename(&source, &tmp).map_err(|e| format!("重命名失败: {e}"))?;
        if let Err(e) = fs::rename(&tmp, &target) {
            let _ = fs::rename(&tmp, &source);
            return Err(format!("重命名失败: {e}"));
        }
    } else {
        fs::rename(&source, &target).map_err(|e| format!("重命名失败: {e}"))?;
    }
    let was_active = read_active(config_path)
        .as_deref()
        .is_some_and(|active| active == old || (case_only && active.eq_ignore_ascii_case(old)));
    if was_active {
        if let Err(e) = write_active_unlocked(config_path, new) {
            // 回滚重命名，避免 active 指向不存在的配置档
            let _ = fs::rename(&target, &source);
            return Err(e);
        }
    }
    Ok(())
}

pub fn delete(config_path: &Path, name: &str) -> Result<(), String> {
    validate_name(name)?;
    if name.eq_ignore_ascii_case(DEFAULT_PROFILE) {
        return Err("不能删除 default 配置档".to_string());
    }
    let _guard = writer::lock_writes();
    ensure_initialized_unlocked(config_path)?;
    let path = profile_path(config_path, name);
    if !path.exists() {
        return Err(format!("配置档「{name}」不存在"));
    }
    if let Some(active) = read_active(config_path) {
        let active_path = profile_path(config_path, &active);
        if active == name || same_path(&path, &active_path) {
            return Err("不能删除当前激活的配置档，请先切换到其它配置档".to_string());
        }
    }
    fs::remove_file(&path).map_err(|e| format!("删除失败: {e}"))
}

/// 在大小写不敏感文件系统上解析磁盘上真实存在的配置档名，避免 active 与文件名大小写漂移
fn resolve_existing_name(config_path: &Path, requested: &str) -> Option<(String, PathBuf)> {
    let candidate = profile_path(config_path, requested);
    if !candidate.exists() {
        return None;
    }
    if let Ok(entries) = fs::read_dir(profiles_dir(config_path)) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.extension().and_then(|e| e.to_str()) == Some("toml") && same_path(&p, &candidate) {
                if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                    return Some((stem.to_string(), p));
                }
            }
        }
    }
    Some((requested.to_string(), candidate))
}

/// 校验目标配置档并返回其文本（实际替换由调用方在持有 lifecycle 锁时完成）
pub fn switch(config_path: &Path, name: &str) -> Result<String, String> {
    validate_name(name)?;
    let _guard = writer::lock_writes();
    // 若 profiles 目录尚未建立才尝试初始化；已建立时允许在降级态下直接切回完好的配置档恢复
    if !profiles_dir(config_path).exists() || !meta_path(config_path).exists() {
        ensure_initialized_unlocked(config_path)?;
    }
    let Some((_, path)) = resolve_existing_name(config_path, name) else {
        return Err(format!("配置档「{name}」不存在"));
    };
    let text = fs::read_to_string(&path).map_err(|e| format!("读取配置档失败: {e}"))?;
    validated_text(&text)?;
    Ok(text)
}

/// 切换配置档：先写 `profiles.toml`，再替换权威的 `config.toml`。
///
/// 顺序是刻意的：`write_active` 失败时文件完全没动；
/// `config.toml` 写入失败时回滚 active。这样两者不会长期不一致。
pub fn switch_to(config_path: &Path, name: &str) -> Result<String, String> {
    validate_name(name)?;
    let _guard = writer::lock_writes();
    if !profiles_dir(config_path).exists() || !meta_path(config_path).exists() {
        ensure_initialized_unlocked(config_path)?;
    }
    let Some((canonical_name, path)) = resolve_existing_name(config_path, name) else {
        return Err(format!("配置档「{name}」不存在"));
    };
    let text = fs::read_to_string(&path).map_err(|e| format!("读取配置档失败: {e}"))?;
    validated_text(&text)?;

    let previous = read_active(config_path);
    // 先备份：备份失败时 profiles.toml 还没动。
    // 若先写 active 再备份，中途失败会留下「active 指向新档、config 还是旧内容」，
    // 下一次 mirror 就会把旧内容写进新档，把用户刚切的配置毁掉。
    writer::backup(config_path)?;
    write_active_unlocked(config_path, &canonical_name)?;
    if let Err(e) = writer::write_atomic_unlocked(config_path, &text) {
        if let Some(previous) = previous {
            if let Err(revert) = write_active_unlocked(config_path, &previous) {
                tracing::warn!("回滚 profiles.toml 失败: {revert}");
            }
        }
        return Err(e);
    }
    Ok(text)
}

/// 在同一把写锁内读取当前激活配置档与 `config.toml` 并完成镜像，避免与重命名并发竞态
pub fn mirror_active(config_path: &Path) -> Result<(), String> {
    let (enabled, _) = profiles_enabled(Some(config_path));
    if !enabled {
        return Ok(());
    }
    mirror_current(config_path)
}

pub(crate) fn mirror_current(config_path: &Path) -> Result<(), String> {
    let _guard = writer::lock_writes();
    ensure_initialized_unlocked(config_path)?;
    let active = read_active(config_path).unwrap_or_else(|| DEFAULT_PROFILE.to_string());
    validate_name(&active)?;
    let text = match fs::read_to_string(config_path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(format!("读取当前配置失败: {e}")),
    };
    let path = profile_path(config_path, &active);
    writer::write_atomic_unlocked(&path, &text)
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
    fn cannot_delete_default_profile() {
        let config = setup("delete-default");
        fs::write(&config, AppConfig::default().to_toml_string().unwrap()).unwrap();
        ensure_initialized(&config).unwrap();
        assert!(delete(&config, DEFAULT_PROFILE).is_err());
        let _ = fs::remove_dir_all(config.parent().unwrap());
    }

    #[test]
    fn switch_to_updates_active_and_config_together() {
        let config = setup("switch-to");
        fs::write(&config, AppConfig::default().to_toml_string().unwrap()).unwrap();
        create(&config, "vr", None).unwrap();

        let text = switch_to(&config, "vr").unwrap();
        assert_eq!(read_active(&config).as_deref(), Some("vr"));
        assert_eq!(fs::read_to_string(&config).unwrap(), text);
        let _ = fs::remove_dir_all(config.parent().unwrap());
    }

    #[test]
    fn new_profile_uses_shipped_default_with_comments() {
        let config = setup("seed");
        fs::write(&config, AppConfig::default().to_toml_string().unwrap()).unwrap();
        create(&config, "fresh", None).unwrap();
        let text = fs::read_to_string(profile_path(&config, "fresh")).unwrap();
        assert!(text.contains('#'), "新建配置档应保留注释: {text}");
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

    #[test]
    fn rename_rejects_default_and_allows_case_only_rename() {
        let config = setup("rename-edge");
        fs::write(&config, AppConfig::default().to_toml_string().unwrap()).unwrap();
        ensure_initialized(&config).unwrap();

        assert!(
            rename(&config, "default", "other").is_err(),
            "default 配置档不应允许重命名"
        );

        create(&config, "vr", None).unwrap();
        switch_to(&config, "vr").unwrap();
        rename(&config, "vr", "VR").unwrap();
        assert_eq!(read_active(&config).as_deref(), Some("VR"));
        assert!(profile_path(&config, "VR").exists());

        // 修改 config.toml 后调用 mirror_current，应镜像到重命名后的 VR.toml
        let mut updated = AppConfig::default();
        updated.subtitle.font_size = 42;
        let updated_text = updated.to_toml_string().unwrap();
        fs::write(&config, &updated_text).unwrap();
        mirror_current(&config).unwrap();
        assert_eq!(
            fs::read_to_string(profile_path(&config, "VR")).unwrap(),
            updated_text
        );

        let entries: Vec<String> = fs::read_dir(profiles_dir(&config))
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert!(
            entries.iter().any(|n| n == "VR.toml"),
            "目录项大小写应更新为 VR.toml，实际: {entries:?}"
        );
        let _ = fs::remove_dir_all(config.parent().unwrap());
    }

    #[test]
    fn delete_blocks_active_profile_even_with_different_case_on_insensitive_fs() {
        let config = setup("delete-active-case");
        fs::write(&config, AppConfig::default().to_toml_string().unwrap()).unwrap();
        ensure_initialized(&config).unwrap();
        create(&config, "vr", None).unwrap();
        switch_to(&config, "vr").unwrap();

        let same_fs = same_path(&profile_path(&config, "vr"), &profile_path(&config, "VR"));
        if same_fs {
            assert!(
                delete(&config, "VR").is_err(),
                "大小写不敏感文件系统上不应允许通过不同大小写删除当前激活档"
            );
            assert!(profile_path(&config, "vr").exists());
        }
        let _ = fs::remove_dir_all(config.parent().unwrap());
    }

    #[test]
    fn list_status_without_init_does_not_seed_broken_config_into_default_profile() {
        let config = setup("degraded-no-init");
        fs::write(&config, "broken toml [[[").unwrap();

        let status = list_status_with_init(Some(&config), false);
        assert!(!status.enabled);
        assert!(!profiles_dir(&config).exists());
        assert!(!meta_path(&config).exists());

        // 即使直接调用 create / ensure_initialized，也不得把损坏的 config.toml 拷入 default.toml
        assert!(create(&config, "vr", None).is_err());
        assert!(!profiles_dir(&config).join("default.toml").exists());

        let _ = fs::remove_dir_all(config.parent().unwrap());
    }

    #[test]
    fn switch_to_preserves_canonical_case_on_insensitive_fs() {
        let config = setup("switch-case");
        fs::write(&config, AppConfig::default().to_toml_string().unwrap()).unwrap();
        ensure_initialized(&config).unwrap();
        create(&config, "vr", None).unwrap();

        if same_path(&profile_path(&config, "vr"), &profile_path(&config, "VR")) {
            switch_to(&config, "VR").unwrap();
            assert_eq!(read_active(&config).as_deref(), Some("vr"));
        }
        assert!(validate_name("COM\u{00B9}").is_err());
        let _ = fs::remove_dir_all(config.parent().unwrap());
    }
}

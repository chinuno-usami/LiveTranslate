//! SteamVR Input 2.0 静态资源：`include_bytes!` 内嵌，首次解压到活动配置目录旁的版本化路径。
//!
//! 解压根目录：`<活动配置文件父目录>/steamvr/<CARGO_PKG_VERSION>/`
//! （便携 = exe 旁 `config/`；默认 = OS 用户配置目录；不可写则回退
//! `data_local_dir()/steamvr/<version>/`）。
//!
//! 仅在 `block_game_input` 走 3A 时需要；3C 拖动不依赖本模块产物。
//!
//! `input.rs` 接入前，非 Windows 构建仅单测引用，故允许 dead_code。

#![allow(dead_code)]

use directories::ProjectDirs;
use std::fs;
use std::path::{Path, PathBuf};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const STAMP_NAME: &str = ".livetranslate-steamvr-version";

struct EmbeddedFile {
    relative: &'static str,
    bytes: &'static [u8],
}

const EMBEDDED: &[EmbeddedFile] = &[
    EmbeddedFile {
        relative: "actions.json",
        bytes: include_bytes!("../../../resources/steamvr/actions.json"),
    },
    EmbeddedFile {
        relative: "steamvr.vrmanifest",
        bytes: include_bytes!("../../../resources/steamvr/steamvr.vrmanifest"),
    },
    EmbeddedFile {
        relative: "bindings/knuckles.json",
        bytes: include_bytes!("../../../resources/steamvr/bindings/knuckles.json"),
    },
    EmbeddedFile {
        relative: "bindings/vive_controller.json",
        bytes: include_bytes!("../../../resources/steamvr/bindings/vive_controller.json"),
    },
    EmbeddedFile {
        relative: "bindings/oculus_touch.json",
        bytes: include_bytes!("../../../resources/steamvr/bindings/oculus_touch.json"),
    },
    EmbeddedFile {
        relative: "bindings/holographic_controller.json",
        bytes: include_bytes!("../../../resources/steamvr/bindings/holographic_controller.json"),
    },
];

/// 解压后交给 SteamVR 的绝对路径集合。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SteamVrAssetPaths {
    /// `.../steamvr/<version>/` 根目录（绝对路径）
    pub root: PathBuf,
    /// `actions.json` 绝对路径（`SetActionManifestPath`）
    pub actions_manifest: PathBuf,
    /// `steamvr.vrmanifest` 绝对路径（`AddApplicationManifest`）
    pub vrmanifest: PathBuf,
}

impl SteamVrAssetPaths {
    fn from_root(root: PathBuf) -> Self {
        let actions_manifest = root.join("actions.json");
        let vrmanifest = root.join("steamvr.vrmanifest");
        Self {
            root,
            actions_manifest,
            vrmanifest,
        }
    }
}

/// 按活动配置路径解压内嵌 SteamVR 资源，返回稳定绝对路径。
///
/// `active_config_path` 为当前活动配置文件（如便携 `config/default.toml` 或
/// 用户 `config.toml`）。`None` 时回落到 OS 用户配置目录作为父目录。
pub fn ensure_extracted(active_config_path: Option<&Path>) -> Result<SteamVrAssetPaths, String> {
    let primary = primary_steamvr_dir(active_config_path)?;
    match extract_into(&primary) {
        Ok(paths) => Ok(paths),
        Err(primary_err) => {
            let fallback = fallback_steamvr_dir()?;
            if fallback == primary {
                return Err(primary_err);
            }
            extract_into(&fallback).map_err(|fallback_err| {
                format!(
                    "SteamVR 资源解压失败（主目录 {}: {primary_err}；回退 {}: {fallback_err}）",
                    primary.display(),
                    fallback.display()
                )
            })
        }
    }
}

fn primary_steamvr_dir(active_config_path: Option<&Path>) -> Result<PathBuf, String> {
    let base = match active_config_path {
        Some(path) => config_parent_dir(path),
        None => crate::config::AppConfig::user_config_dir().map_err(|e| e.to_string())?,
    };
    Ok(base.join("steamvr").join(VERSION))
}

fn fallback_steamvr_dir() -> Result<PathBuf, String> {
    ProjectDirs::from("com", "chinuno", "LiveTranslate")
        .map(|dirs| dirs.data_local_dir().join("steamvr").join(VERSION))
        .ok_or_else(|| "无法确定 data_local_dir 作为 SteamVR 资源回退目录".to_string())
}

fn config_parent_dir(config_path: &Path) -> PathBuf {
    match config_path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

fn extract_into(target_dir: &Path) -> Result<SteamVrAssetPaths, String> {
    let absolute_root = to_absolute(target_dir);
    if is_current(&absolute_root) {
        return Ok(SteamVrAssetPaths::from_root(absolute_root));
    }

    fs::create_dir_all(&absolute_root).map_err(|e| {
        format!(
            "创建 SteamVR 资源目录 {} 失败: {e}",
            absolute_root.display()
        )
    })?;

    for file in EMBEDDED {
        let dest = absolute_root.join(file.relative);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("创建 {} 失败: {e}", parent.display()))?;
        }
        // 先写临时文件再 rename，降低半写入被 SteamVR 读到的窗口
        let tmp = dest.with_extension("tmp");
        fs::write(&tmp, file.bytes).map_err(|e| format!("写入 {} 失败: {e}", tmp.display()))?;
        fs::rename(&tmp, &dest).map_err(|e| {
            let _ = fs::remove_file(&tmp);
            format!("落盘 {} 失败: {e}", dest.display())
        })?;
    }

    let stamp_path = absolute_root.join(STAMP_NAME);
    let stamp_tmp = absolute_root.join(format!("{STAMP_NAME}.tmp"));
    fs::write(&stamp_tmp, VERSION).map_err(|e| format!("写入版本戳临时文件失败: {e}"))?;
    fs::rename(&stamp_tmp, &stamp_path).map_err(|e| {
        let _ = fs::remove_file(&stamp_tmp);
        format!("写入版本戳失败: {e}")
    })?;

    if !is_current(&absolute_root) {
        return Err(format!(
            "SteamVR 资源解压后校验失败: {}",
            absolute_root.display()
        ));
    }

    Ok(SteamVrAssetPaths::from_root(absolute_root))
}

fn is_current(dir: &Path) -> bool {
    let Ok(stamp) = fs::read_to_string(dir.join(STAMP_NAME)) else {
        return false;
    };
    if stamp.trim() != VERSION {
        return false;
    }
    EMBEDDED
        .iter()
        .all(|file| dir.join(file.relative).is_file())
}

fn to_absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_dir(tag: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "livetranslate-steamvr-assets-{}-{}-{}-{}",
            std::process::id(),
            nanos,
            n,
            tag
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create test dir");
        dir
    }

    fn parse_json_file(path: &Path) -> Value {
        let text = fs::read_to_string(path).unwrap_or_else(|e| {
            panic!("read {}: {e}", path.display());
        });
        serde_json::from_str(&text).unwrap_or_else(|e| {
            panic!("parse {}: {e}", path.display());
        })
    }

    fn assert_actions_only_grip_trigger(actions: &Value) {
        let names: Vec<&str> = actions["actions"]
            .as_array()
            .expect("actions array")
            .iter()
            .map(|a| a["name"].as_str().expect("name"))
            .collect();
        for name in &names {
            assert!(
                name.contains("grip") || name.contains("trigger"),
                "action 仅允许 grip/trigger: {name}"
            );
            for banned in ["joystick", "trackpad", "thumbstick", "menu"] {
                assert!(
                    !name.contains(banned),
                    "action 不得包含非 grip/trigger: {name}"
                );
            }
        }
    }

    fn assert_binding_sources_only_grip_trigger(binding: &Value) {
        let sources = binding["bindings"]["/actions/move"]["sources"]
            .as_array()
            .expect("binding sources");
        for source in sources {
            let path = source["path"].as_str().expect("source path").to_lowercase();
            assert!(
                path.contains("/input/grip") || path.contains("/input/trigger"),
                "binding 路径仅允许 grip/trigger: {path}"
            );
            for banned in [
                "/input/joystick",
                "/input/trackpad",
                "/input/thumbstick",
                "/input/menu",
                "application_menu",
            ] {
                assert!(!path.contains(banned), "binding 不得绑定 {banned}: {path}");
            }
        }
    }

    #[test]
    fn extracts_under_config_parent_with_version_stamp_and_absolute_paths() {
        let base = test_dir("primary");
        let config_path = base.join("default.toml");
        fs::write(&config_path, "# test\n").unwrap();

        let paths = ensure_extracted(Some(&config_path)).expect("extract");
        assert!(paths.root.is_absolute());
        assert!(paths.actions_manifest.is_absolute());
        assert!(paths.vrmanifest.is_absolute());
        assert_eq!(paths.root, to_absolute(&base.join("steamvr").join(VERSION)));
        assert_eq!(paths.actions_manifest, paths.root.join("actions.json"));
        assert_eq!(paths.vrmanifest, paths.root.join("steamvr.vrmanifest"));
        assert_eq!(
            fs::read_to_string(paths.root.join(STAMP_NAME))
                .unwrap()
                .trim(),
            VERSION
        );

        let actions = parse_json_file(&paths.actions_manifest);
        assert_actions_only_grip_trigger(&actions);
        let action_names: Vec<&str> = actions["actions"]
            .as_array()
            .expect("actions array")
            .iter()
            .map(|a| a["name"].as_str().expect("name"))
            .collect();
        assert_eq!(
            action_names,
            [
                "/actions/move/in/grip_left",
                "/actions/move/in/grip_right",
                "/actions/move/in/trigger_left",
                "/actions/move/in/trigger_right",
            ]
        );

        for name in [
            "bindings/knuckles.json",
            "bindings/vive_controller.json",
            "bindings/oculus_touch.json",
            "bindings/holographic_controller.json",
        ] {
            let binding = parse_json_file(&paths.root.join(name));
            assert_binding_sources_only_grip_trigger(&binding);
        }

        let manifest = parse_json_file(&paths.vrmanifest);
        assert_eq!(
            manifest["applications"][0]["app_key"],
            "com.chinuno.livetranslate"
        );
        assert_eq!(
            manifest["applications"][0]["action_manifest_path"],
            "actions.json"
        );
    }

    #[test]
    fn second_extract_is_idempotent_when_stamp_matches() {
        let base = test_dir("idempotent");
        let config_path = base.join("config.toml");
        fs::write(&config_path, "").unwrap();

        let first = ensure_extracted(Some(&config_path)).unwrap();
        #[cfg(unix)]
        let mtime_before = {
            use std::os::unix::fs::MetadataExt;
            fs::metadata(first.root.join(STAMP_NAME)).unwrap().mtime()
        };

        // 稍等保证若误写会变 mtime（多数 FS 秒级分辨率）
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let second = ensure_extracted(Some(&config_path)).unwrap();
        assert_eq!(first, second);

        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let mtime_after = fs::metadata(second.root.join(STAMP_NAME)).unwrap().mtime();
            assert_eq!(mtime_before, mtime_after, "匹配版本戳时不应重写");
        }
    }

    #[test]
    fn version_stamp_mismatch_overwrites_files() {
        let base = test_dir("stamp-overwrite");
        let config_path = base.join("config.toml");
        fs::write(&config_path, "").unwrap();
        let paths = ensure_extracted(Some(&config_path)).unwrap();

        let actions_path = paths.actions_manifest.clone();
        fs::write(&actions_path, b"{\"actions\":[]}").unwrap();
        fs::write(paths.root.join(STAMP_NAME), "0.0.0-old").unwrap();

        let refreshed = ensure_extracted(Some(&config_path)).unwrap();
        assert_eq!(refreshed.root, paths.root);
        let actions = parse_json_file(&refreshed.actions_manifest);
        assert!(
            actions["actions"]
                .as_array()
                .map(|a| !a.is_empty())
                .unwrap_or(false),
            "版本戳不匹配时应覆盖回内嵌资源"
        );
        assert_eq!(
            fs::read_to_string(refreshed.root.join(STAMP_NAME))
                .unwrap()
                .trim(),
            VERSION
        );
    }

    #[cfg(unix)]
    #[test]
    fn falls_back_when_primary_directory_not_writable() {
        let primary_base = test_dir("readonly-primary");
        let fallback_base = test_dir("fallback-data-local");
        let config_path = primary_base.join("default.toml");
        fs::write(&config_path, "").unwrap();

        // 锁住父目录，使 steamvr/<version> 无法创建
        let mut perms = fs::metadata(&primary_base).unwrap().permissions();
        perms.set_readonly(true);
        fs::set_permissions(&primary_base, perms).unwrap();

        let primary = primary_base.join("steamvr").join(VERSION);
        let fallback = fallback_base.join("steamvr").join(VERSION);
        let result = extract_into(&primary).or_else(|_| extract_into(&fallback));

        // 恢复权限以便清理
        let mut perms = fs::metadata(&primary_base).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
        fs::set_permissions(&primary_base, perms).unwrap();

        let paths = result.expect("fallback extract");
        assert_eq!(paths.root, to_absolute(&fallback));
        assert!(paths.actions_manifest.is_file());
    }

    #[test]
    fn embedded_bytes_match_repo_resources() {
        for file in EMBEDDED {
            let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("resources/steamvr")
                .join(file.relative);
            let disk = fs::read(&repo).unwrap_or_else(|e| {
                panic!("read {}: {e}", repo.display());
            });
            assert_eq!(
                disk, file.bytes,
                "include_bytes 与仓库资源不一致: {}",
                file.relative
            );
        }
    }
}

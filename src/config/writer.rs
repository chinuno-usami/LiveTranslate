//! 配置写入层：使用 `toml_edit` 定点修改，保留注释与排版。
//!
//! 所有写文件操作都：校验 → 备份 → 写临时文件 → 原子替换；
//! 校验失败时文件不会被改动，避免把用户配置写坏。
//! 进程内写入用全局锁串行化，临时文件名包含 pid 与序号。

use crate::config::{AppConfig, AudioSourceConfig};
use serde_json::Value as JsonValue;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use toml_edit::{Array, ArrayOfTables, DocumentMut, InlineTable, Item, Table, Value};

/// 进程内写配置的串行化锁。
///
/// 配置写入是「读 → 改 → 备份 → 落盘」的复合操作，而 Tauri 命令在 tokio
/// 多线程运行时上并发执行。没有这把锁时两个写入者会共用临时文件并互相覆盖。
static WRITE_LOCK: Mutex<()> = Mutex::new(());

/// 临时文件名去重计数（保证并发写入者不会撞名）
static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

pub(crate) fn lock_writes() -> std::sync::MutexGuard<'static, ()> {
    WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// 读取配置文本（文件不存在时返回空串）
pub fn read_text(path: &Path) -> Result<String, String> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(format!("读取 {} 失败: {e}", path.display())),
    }
}

/// `toml_edit` 的 `Display` 会把出错行的原文拼进去（可能含 `api_key`），这里只留消息。
fn toml_edit_error_text(e: &toml_edit::TomlError) -> String {
    e.message().to_string()
}

fn tmp_path_for(path: &Path) -> PathBuf {
    let seq = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "config".to_string());
    let tmp_name = format!(".{file_name}.{}-{seq}.tmp", std::process::id());
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(tmp_name),
        _ => PathBuf::from(tmp_name),
    }
}

/// 原子写入：先写同目录临时文件，fsync，再 rename 覆盖
///
/// 保留原文件权限位（配置文件里有 api_key，用户可能已 `chmod 600`）。
/// 调用方需自行持有 [`lock_writes`]（`apply_changes` / `save_raw` 已经在锁内，
/// 不能重复加锁——`std::sync::Mutex` 不可重入）。
pub(crate) fn write_atomic_unlocked(path: &Path, content: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|e| format!("创建目录失败: {e}"))?;
        }
    }
    let tmp = tmp_path_for(path);
    if let Err(e) = write_file_durable(&tmp, content) {
        let _ = fs::remove_file(&tmp);
        return Err(format!("写入临时文件失败: {e}"));
    }
    // 临时文件在 Unix 上已按 0600 创建；若原文件存在则沿用用户自定义权限位
    if let Ok(meta) = fs::metadata(path) {
        let _ = fs::set_permissions(&tmp, meta.permissions());
    }
    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(format!("替换配置文件失败: {e}"));
    }
    // rename 只保证「名字切换」原子，落盘还要同步父目录，
    // 否则掉电后可能留下一个长度为 0 的配置文件（里面存着 api_key）。
    sync_parent_dir(path);
    Ok(())
}

fn write_file_durable(path: &Path, content: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // 配置含 api_key，创建即收紧为 0600，消除先 0644 后 chmod 的明文暴露窗口
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(content.as_bytes())?;
    file.sync_all()?;
    Ok(())
}

#[cfg(unix)]
fn sync_parent_dir(path: &Path) {
    if let Some(parent) = path.parent() {
        if let Ok(dir) = fs::File::open(parent) {
            let _ = dir.sync_all();
        }
    }
}

#[cfg(not(unix))]
fn sync_parent_dir(_path: &Path) {}

/// 原子写入（自行加锁）。供单测直接调用。
#[cfg(test)]
pub fn write_atomic(path: &Path, content: &str) -> Result<(), String> {
    let _guard = lock_writes();
    write_atomic_unlocked(path, content)
}

/// 备份到 `<file>.bak`，返回备份路径（源文件不存在时不备份）
pub fn backup(path: &Path) -> Result<Option<PathBuf>, String> {
    if !path.exists() {
        return Ok(None);
    }
    let mut name = path.as_os_str().to_os_string();
    name.push(".bak");
    let backup = PathBuf::from(name);
    fs::copy(path, &backup).map_err(|e| format!("备份配置失败: {e}"))?;
    Ok(Some(backup))
}

/// 应用一批 `(dotted.path, json value)` 修改，并校验结果。
///
/// 先校验、后落盘：校验失败时文件一个字节也不会变。
/// 写入成功后重新解析得到的配置会返回给调用方，避免调用方再自己拼一份。
pub fn apply_changes(path: &Path, changes: &[(String, JsonValue)]) -> Result<AppConfig, String> {
    let _guard = lock_writes();
    let original = read_text(path)?;
    let mut doc: DocumentMut = if original.trim().is_empty() {
        AppConfig::default()
            .to_toml_string()
            .map_err(|e| format!("生成默认配置失败: {e}"))?
            .parse()
            .map_err(|e| format!("解析默认配置失败: {}", toml_edit_error_text(&e)))?
    } else {
        original
            .parse()
            .map_err(|e| format!("解析 {} 失败: {}", path.display(), toml_edit_error_text(&e)))?
    };

    for (dotted, value) in changes {
        if dotted == "audio.sources" {
            let sources = parse_sources(value)?;
            set_sources(&mut doc, &sources);
        } else if dotted == "translate.extra_body" {
            // 设置页用 TOML 文本编辑 extra_body，这里先解析成结构化值
            let json = match value {
                JsonValue::String(text) => {
                    let parsed: toml::Value = if text.trim().is_empty() {
                        toml::Value::Table(Default::default())
                    } else {
                        toml::from_str(text).map_err(|e| {
                            format!(
                                "extra_body TOML 解析失败: {}",
                                crate::config::toml_error_text(&e)
                            )
                        })?
                    };
                    serde_json::to_value(parsed).map_err(|e| format!("extra_body 转换失败: {e}"))?
                }
                other => other.clone(),
            };
            set_path(&mut doc, dotted, json_to_item(&json)?)?;
        } else {
            set_path(&mut doc, dotted, json_to_item(value)?)?;
        }
    }

    let updated = doc.to_string();
    // 先校验，再落盘：非法值不进入写入路径
    let cfg = AppConfig::from_str(&updated).map_err(|e| format!("写入的配置无效，未写盘: {e}"))?;
    cfg.validate()
        .map_err(|e| format!("写入的配置无效，未写盘: {e}"))?;
    // 回读一致性校验：捕获拼错/不存在的配置项（serde 会默默忽略未知键）
    for (dotted, value) in changes {
        verify_change_applied(&cfg, dotted, value)?;
    }

    backup(path)?;
    write_atomic_unlocked(path, &updated)?;
    Ok(cfg)
}

/// 收集 TOML 里 schema 不认识的配置项。
///
/// `serde` 会默默忽略未知键，所以原始配置编辑器写 `[osc]`、
/// `max_line = 4` 这类拼写错误时会「保存成功但从来没生效」。
/// 返回的点分路径用于提示用户（不阻止保存：历史配置里可能真有旧字段）。
pub fn unknown_keys(text: &str) -> Vec<String> {
    let Ok(root) = text.parse::<toml::Value>() else {
        return Vec::new();
    };
    let known: std::collections::HashSet<String> =
        crate::config::schema::build(&AppConfig::default())
            .groups
            .iter()
            .flat_map(|group| group.fields.iter())
            .map(|field| field.path.trim_end_matches("[]").to_string())
            .collect();
    let mut found = Vec::new();
    collect_unknown(&root, "", &known, &mut found);
    found
}

fn collect_unknown(
    value: &toml::Value,
    prefix: &str,
    known: &std::collections::HashSet<String>,
    out: &mut Vec<String>,
) {
    let Some(table) = value.as_table() else {
        return;
    };
    for (key, child) in table {
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        // 这两处的键名由程序结构化处理，不做白名单
        if path == "audio.sources" || path == "translate.extra_body" {
            continue;
        }
        if child.is_table() {
            collect_unknown(child, &path, known, out);
            continue;
        }
        let is_known = known.contains(&path)
            || known
                .iter()
                .any(|known| known.starts_with(&format!("{path}.")));
        if !is_known {
            out.push(path);
        }
    }
}

/// 确认某个改动真的落到了结构体里（而不是被 serde 当作未知键丢掉）
fn verify_change_applied(cfg: &AppConfig, dotted: &str, value: &JsonValue) -> Result<(), String> {
    // 这两项在写入时会被重写成结构化形态，取值语义不同，跳过比对
    if dotted == "audio.sources" || dotted == "translate.extra_body" {
        return Ok(());
    }
    let actual = crate::config::schema::get_value(cfg, dotted)
        .ok_or_else(|| format!("未知配置项: {dotted}"))?;
    if json_equivalent(&actual, value) {
        Ok(())
    } else {
        Err(format!(
            "配置项 {dotted} 写入后值不一致（期望 {value}，实际 {actual}）"
        ))
    }
}

/// 浮点比较必须用相对容差。
///
/// 配置里的浮点字段是 `f32`（0.93 / 0.9 / 0.85 …），写盘回读后是
/// `f64::from(f32)`，与前端发来的 `f64` 天然差 1e-8 量级；用绝对 1e-9 会把
/// 几乎所有浮点配置项判成「不一致」，设置页保存直接失败。
fn floats_close(x: f64, y: f64) -> bool {
    if x == y {
        return true;
    }
    let diff = (x - y).abs();
    let scale = x.abs().max(y.abs()).max(1.0);
    diff <= 1e-6 * scale
}

fn json_equivalent(a: &JsonValue, b: &JsonValue) -> bool {
    match (a, b) {
        (JsonValue::Number(x), JsonValue::Number(y)) => match (x.as_f64(), y.as_f64()) {
            (Some(x), Some(y)) => floats_close(x, y),
            _ => x == y,
        },
        (JsonValue::Array(x), JsonValue::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y.iter()).all(|(x, y)| json_equivalent(x, y))
        }
        (JsonValue::Object(x), JsonValue::Object(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(k, v)| y.get(k).map(|w| json_equivalent(v, w)).unwrap_or(false))
        }
        _ => a == b,
    }
}

/// 直接写入整份 TOML 文本（原始编辑器使用），校验失败不落盘
pub fn save_raw(path: &Path, text: &str) -> Result<AppConfig, String> {
    let _guard = lock_writes();
    let cfg = AppConfig::from_str(text).map_err(|e| format!("TOML 解析失败: {e}"))?;
    cfg.validate()?;
    backup(path)?;
    write_atomic_unlocked(path, text)?;
    Ok(cfg)
}

fn parse_sources(value: &JsonValue) -> Result<Vec<AudioSourceConfig>, String> {
    let array = value
        .as_array()
        .ok_or_else(|| "audio.sources 必须是数组".to_string())?;
    array
        .iter()
        .map(|item| {
            serde_json::from_value::<AudioSourceConfig>(item.clone())
                .map_err(|e| format!("解析输入源失败: {e}"))
        })
        .collect()
}

fn set_sources(doc: &mut DocumentMut, sources: &[AudioSourceConfig]) {
    let mut tables = ArrayOfTables::new();
    for source in sources {
        let mut table = Table::new();
        table["id"] = toml_edit::value(source.id.clone());
        table["label"] = toml_edit::value(source.label.clone());
        table["device"] = toml_edit::value(source.device.clone());
        table["enabled"] = toml_edit::value(source.enabled);
        table["translate"] = toml_edit::value(source.translate);
        table["language"] = toml_edit::value(source.language.clone());
        tables.push(table);
    }
    let audio = doc
        .entry("audio")
        .or_insert_with(|| Item::Table(Table::new()))
        .as_table_mut();
    if let Some(audio) = audio {
        audio.set_dotted(false);
        // 若之前被写成普通数组，直接替换为 array of tables
        audio.insert("sources", Item::ArrayOfTables(tables));
    } else {
        let mut table = Table::new();
        table.insert("sources", Item::ArrayOfTables(tables));
        doc.insert("audio", Item::Table(table));
    }
}

fn set_path(doc: &mut DocumentMut, dotted: &str, mut item: Item) -> Result<(), String> {
    let parts: Vec<&str> = dotted.split('.').filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        return Err("配置路径为空".to_string());
    }

    let mut table: &mut Table = doc.as_table_mut();
    for part in &parts[..parts.len() - 1] {
        if !table.contains_key(part) {
            table.insert(part, Item::Table(Table::new()));
        }
        let entry = table
            .get_mut(part)
            .ok_or_else(|| format!("无法创建配置段 {part}"))?;
        // 内联表（`section = { k = v }`）展开为标准表，保留已有键；
        // 数组表无法定点下钻，直接报错而不是默默清空。
        match entry {
            Item::Table(_) => {}
            Item::Value(Value::InlineTable(inline)) => {
                let mut expanded = Table::new();
                for (key, value) in inline.iter() {
                    expanded.insert(key, Item::Value(value.clone()));
                }
                *entry = Item::Table(expanded);
            }
            Item::ArrayOfTables(_) => {
                return Err(format!("配置段 {part} 是数组表，不支持定点修改"));
            }
            _ => *entry = Item::Table(Table::new()),
        }
        table = entry
            .as_table_mut()
            .ok_or_else(|| format!("配置段 {part} 不是表"))?;
    }

    let key = parts[parts.len() - 1];

    // 原地替换以保留 Key 上的注释（prefix decor）；否则设置页保存一次就会丢注释
    if let Some((_key, existing)) = table.get_key_value_mut(key) {
        match (&mut *existing, &mut item) {
            (Item::Value(old), Item::Value(new_value)) => {
                *new_value.decor_mut() = old.decor().clone();
            }
            (Item::Table(old), Item::Value(Value::InlineTable(new_table))) => {
                // 已有 `[section]` 表时改为原地更新，保留表头注释与位置
                old.set_dotted(false);
                let entries: Vec<(String, Value)> = new_table
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.clone()))
                    .collect();
                old.retain(|k, _| entries.iter().any(|(name, _)| name == k));
                for (name, value) in entries {
                    old.insert(&name, Item::Value(value));
                }
                return Ok(());
            }
            _ => {}
        }
        *existing = item;
    } else {
        table.insert(key, item);
    }
    Ok(())
}

fn json_to_item(value: &JsonValue) -> Result<Item, String> {
    Ok(match value {
        JsonValue::Bool(v) => Item::Value(Value::from(*v)),
        JsonValue::Number(n) => {
            if let Some(i) = n.as_i64() {
                Item::Value(Value::from(i))
            } else if let Some(f) = n.as_f64() {
                Item::Value(Value::from(f))
            } else {
                return Err(format!("不支持的数字: {n}"));
            }
        }
        JsonValue::String(s) => Item::Value(Value::from(s.clone())),
        JsonValue::Array(items) => {
            let mut array = Array::new();
            for item in items {
                let value = json_to_value(item)?;
                array.push_formatted(value);
            }
            Item::Value(Value::Array(array))
        }
        JsonValue::Object(map) => {
            // 对象优先写成内联表，保持 `extra_body = { ... }` 紧凑可读
            let mut inline = InlineTable::new();
            for (key, value) in map {
                inline.insert(key, json_to_value(value)?);
            }
            Item::Value(Value::InlineTable(inline))
        }
        JsonValue::Null => return Err("不支持 null 值".to_string()),
    })
}

fn json_to_value(value: &JsonValue) -> Result<Value, String> {
    match value {
        JsonValue::Bool(v) => Ok(Value::from(*v)),
        JsonValue::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(Value::from(i))
            } else if let Some(f) = n.as_f64() {
                Ok(Value::from(f))
            } else {
                Err(format!("不支持的数字: {n}"))
            }
        }
        JsonValue::String(s) => Ok(Value::from(s.clone())),
        JsonValue::Array(items) => {
            let mut array = Array::new();
            for item in items {
                array.push_formatted(json_to_value(item)?);
            }
            Ok(Value::Array(array))
        }
        JsonValue::Object(map) => {
            let mut inline = InlineTable::new();
            for (key, value) in map {
                inline.insert(key, json_to_value(value)?);
            }
            Ok(Value::InlineTable(inline))
        }
        JsonValue::Null => Err("不支持 null 值".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn temp_path(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "livetranslate-writer-{}-{}",
            std::process::id(),
            tag
        ));
        let _ = fs::remove_dir_all(&dir);
        dir.join("config.toml")
    }

    fn seed(path: &Path) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        // 真实用户配置是完整配置，这里用默认配置生成后再插入注释
        let text = AppConfig::default()
            .to_toml_string()
            .unwrap()
            .replace("font_size = 28", "# 字号\nfont_size = 28")
            .replace("api_key = \"YOUR_API_KEY\"", "api_key = \"sk-old\"");
        fs::write(path, format!("# 顶部注释\n{text}")).unwrap();
    }

    /// 浮点字段是 f32，前端发来的是 f64；回读校验不能用绝对容差。
    /// 回归：0.93、0.9、0.85 这些刻度值会被判成「不一致」，整批保存失败。
    #[test]
    fn accepts_f32_quantized_floats() {
        let path = temp_path("float");
        seed(&path);
        for (path_key, value) in [
            ("translate.temperature", json!(0.93)),
            ("audio.silence_threshold", json!(0.15)),
            ("output.steamvr.opacity", json!(0.85)),
            ("output.steamvr.line_alphas", json!([1.0, 0.72, 0.48])),
            ("output.steamvr.world_position", json!([0.0, 1.35, -2.4])),
        ] {
            apply_changes(&path, &[(path_key.to_string(), value)])
                .unwrap_or_else(|e| panic!("{path_key} 应可保存: {e}"));
        }
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    /// 随包默认配置必须全部是「已知键」，否则新装用户的原始编辑器会立刻报一堆未知项
    #[test]
    fn shipped_default_has_no_unknown_keys() {
        assert_eq!(
            unknown_keys(AppConfig::SHIPPED_DEFAULT),
            Vec::<String>::new()
        );
    }

    /// 拼错的键必须被点名（serde 会静默忽略）
    #[test]
    fn unknown_keys_are_reported() {
        let mut text = AppConfig::default().to_toml_string().unwrap();
        text.push_str("\n[osc]\nmax_line = 4\n");
        let unknown = unknown_keys(&text);
        assert_eq!(unknown, vec!["osc.max_line".to_string()]);
    }

    #[test]
    fn keeps_comments_and_updates_scalar() {
        let path = temp_path("comment");
        seed(&path);
        let cfg = apply_changes(&path, &[("subtitle.font_size".into(), json!(40))]).unwrap();
        assert_eq!(cfg.subtitle.font_size, 40);
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("# 顶部注释"));
        assert!(text.contains("# 字号"));
        assert!(text.contains("font_size = 40"));
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn writes_sources_array_of_tables() {
        let path = temp_path("sources");
        seed(&path);
        let sources = vec![
            AudioSourceConfig::new("mic", "麦克风", "mic:0"),
            AudioSourceConfig {
                enabled: false,
                ..AudioSourceConfig::new("game", "游戏", "loopback:0")
            },
        ];
        let value = serde_json::to_value(&sources).unwrap();
        let cfg = apply_changes(&path, &[("audio.sources".into(), value)]).unwrap();
        assert_eq!(cfg.audio.sources.len(), 2);
        assert!(!cfg.audio.sources[1].enabled);
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("[[audio.sources]]"));
        assert!(text.contains("id = \"game\""));
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn nested_and_inline_table_roundtrip() {
        let path = temp_path("nested");
        seed(&path);
        let value = json!({ "thinking": { "type": "disabled" } });
        apply_changes(&path, &[("translate.extra_body".into(), value)]).unwrap();
        let cfg = AppConfig::from_file(&path).unwrap();
        assert!(cfg.translate.extra_body.is_some());
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("extra_body"));
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn invalid_value_is_not_written() {
        let path = temp_path("invalid");
        seed(&path);
        let before = fs::read_to_string(&path).unwrap();
        let err =
            apply_changes(&path, &[("output.osc.min_interval_ms".into(), json!(1))]).unwrap_err();
        assert!(err.contains("未写盘"), "实际错误: {err}");
        let after = fs::read_to_string(&path).unwrap();
        assert_eq!(before, after);
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn unknown_path_is_rejected() {
        // serde 会默默忽略未知键，写入层必须自己拦住拼错的配置项
        let path = temp_path("unknown");
        seed(&path);
        let before = fs::read_to_string(&path).unwrap();
        let err = apply_changes(&path, &[("subtitle.fontsize".into(), json!(40))]).unwrap_err();
        assert!(err.contains("未知配置项"), "实际错误: {err}");
        assert_eq!(fs::read_to_string(&path).unwrap(), before, "文件不应被改动");
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn inline_table_is_expanded_not_cleared() {
        // 用户手写的内联表：下钻修改子键时不能把同段其它键静默清掉
        let path = temp_path("inline");
        seed(&path);
        let text = fs::read_to_string(&path).unwrap();
        let text = text.replacen(
            "[translate]\n",
            "[translate]\nextra_body = { thinking = { type = \"disabled\" }, keep = 1 }\n",
            1,
        );
        fs::write(&path, &text).unwrap();

        let cfg = apply_changes(&path, &[("translate.extra_body.keep".into(), json!(2))]).unwrap();
        let extra = cfg.translate.extra_body.expect("extra_body 应保留");
        assert_eq!(extra.get("keep").and_then(|v| v.as_integer()), Some(2));
        assert!(
            extra.get("thinking").is_some(),
            "内联表的其它键不能被清掉: {extra:?}"
        );
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn backup_is_created() {
        let path = temp_path("backup");
        seed(&path);
        let mut expected = path.as_os_str().to_os_string();
        expected.push(".bak");
        backup(&path).unwrap().unwrap();
        assert!(PathBuf::from(expected).exists());
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn save_raw_rejects_invalid_toml() {
        let path = temp_path("raw");
        seed(&path);
        assert!(save_raw(&path, "not toml [").is_err());
        assert!(AppConfig::from_file(&path).is_ok());
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn schema_fields_roundtrip_through_writer() {
        use crate::config::schema;

        let path = temp_path("schema-roundtrip");
        seed(&path);
        let defaults = AppConfig::default();
        let mut expected = defaults.clone();
        let mut changes = Vec::new();
        for group in schema::build(&defaults).groups {
            for field in group.fields {
                if field.path == "audio.sources" {
                    continue;
                }
                schema::apply_change(&mut expected, &field.path, &field.value).unwrap();
                changes.push((
                    field.path.trim_end_matches("[]").to_string(),
                    field.value.clone(),
                ));
            }
        }
        let cfg = apply_changes(&path, &changes).unwrap();

        // 文件里不能残留未知键：重新序列化后应与预期配置一致
        let expected_value = toml::Value::try_from(&expected).unwrap();
        let actual_value = toml::Value::try_from(&cfg).unwrap();
        assert_eq!(actual_value, expected_value);
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn extra_body_accepts_toml_text() {
        let path = temp_path("extra-body");
        seed(&path);
        let cfg = apply_changes(
            &path,
            &[(
                "translate.extra_body".into(),
                json!("thinking = { type = \"disabled\" }"),
            )],
        )
        .unwrap();
        assert!(cfg.translate.extra_body.is_some());
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("extra_body"));
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn edits_shipped_default_preserve_comments() {
        let path = temp_path("shipped");
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, AppConfig::SHIPPED_DEFAULT).unwrap();

        let sources =
            serde_json::to_value(vec![AudioSourceConfig::new("mic", "麦克风", "mic:0")]).unwrap();
        let cfg = apply_changes(
            &path,
            &[
                ("subtitle.font_size".into(), json!(40)),
                ("output.osc.max_chars".into(), json!(100)),
                ("audio.sources".into(), sources),
            ],
        )
        .unwrap();
        assert_eq!(cfg.subtitle.font_size, 40);
        assert_eq!(cfg.output.osc.max_chars, 100);
        assert_eq!(cfg.audio.sources.len(), 1);

        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("# 是否启用 VAD"), "其它段注释应保留");
        assert!(text.contains("font_size = 40"));
        assert!(text.contains("max_chars = 100"));
        assert!(text.contains("[[audio.sources]]"));
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn all_defaults_roundtrip_through_writer() {
        // 保证 writer 产出的文件能被 AppConfig 解析
        let path = temp_path("all");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        write_atomic(&path, &AppConfig::default().to_toml_string().unwrap()).unwrap();
        let cfg = AppConfig::from_file(&path).unwrap();
        cfg.validate().unwrap();
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    /// `[audio]` 段里同时有 `[[audio.sources]]` 时，新增的标量键必须落在
    /// 段头之后、数组表之前，否则会被当成最后一个输入源的字段。
    #[test]
    fn scalar_key_stays_out_of_array_of_tables() {
        let path = temp_path("ordering");
        let sources = vec![AudioSourceConfig::new("mic", "麦克风", "mic:0")];
        apply_changes(
            &path,
            &[(
                "audio.sources".into(),
                serde_json::to_value(&sources).unwrap(),
            )],
        )
        .unwrap();
        let cfg =
            apply_changes(&path, &[("audio.device_name".into(), json!("speaker:1"))]).unwrap();
        assert_eq!(cfg.audio.device_name, "speaker:1");
        let text = fs::read_to_string(&path).unwrap();
        let key = text.find("device_name").expect("应写入 device_name");
        let array = text.find("[[audio.sources]]").expect("应保留数组表");
        assert!(key < array, "device_name 落在了数组表之后:\n{text}");
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }
}

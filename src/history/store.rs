//! 翻译历史本地持久化（JSONL）。
//!
//! 路径：`ProjectDirs::data_local_dir()/history/translations.jsonl`
//! 每行一条 [`TranslationRecord`] JSON；按 `created_at` 倒序分页读取；
//! 超过 [`DEFAULT_MAX_RECORDS`] 时裁掉最旧记录。

use super::TranslationRecord;
use crate::error::{AppError, AppResult};
use directories::ProjectDirs;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// 默认保留上限（条）。
pub const DEFAULT_MAX_RECORDS: usize = 5000;

/// 相对 `data_local_dir` 的历史子目录名。
const HISTORY_DIR_NAME: &str = "history";
/// JSONL 文件名。
const HISTORY_FILE_NAME: &str = "translations.jsonl";

#[derive(Debug, Default)]
struct StoreState {
    /// 已解析记录条数；`None` 表示尚未计数（首次超限检查前会补齐）。
    record_count: Option<usize>,
}

/// 本地 JSONL 历史存储。
///
/// 通过内部 `Mutex` 串行化同一路径上的读写 / 清空，避免 writer 与 UI 命令交错损坏文件。
#[derive(Debug, Clone)]
pub struct HistoryStore {
    path: PathBuf,
    max_records: usize,
    state: Arc<Mutex<StoreState>>,
}

impl HistoryStore {
    /// 使用默认路径与保留上限。
    pub fn open_default() -> AppResult<Self> {
        Ok(Self {
            path: default_history_path()?,
            max_records: DEFAULT_MAX_RECORDS,
            state: Arc::new(Mutex::new(StoreState::default())),
        })
    }

    /// 指定文件路径（测试或自定义）。
    #[allow(dead_code)] // 单测与后续自定义路径入口
    pub fn with_path(path: impl Into<PathBuf>, max_records: usize) -> Self {
        Self {
            path: path.into(),
            max_records: max_records.max(1),
            state: Arc::new(Mutex::new(StoreState::default())),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn max_records(&self) -> usize {
        self.max_records
    }

    /// 追加一条记录；仅在计数超过 `max_records` 时全量裁剪重写。
    pub fn append(&self, record: &TranslationRecord) -> AppResult<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| AppError::Config("翻译历史存储锁已损坏".into()))?;
        self.ensure_parent()?;
        {
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)?;
            serde_json::to_writer(&mut file, record)?;
            file.write_all(b"\n")?;
            file.flush()?;
        }
        let next = match state.record_count {
            Some(count) => count + 1,
            None => self.count_records()?,
        };
        state.record_count = Some(next);
        if next > self.max_records {
            self.trim_to_max_locked(&mut state)?;
        }
        Ok(())
    }

    /// 按 `created_at` 倒序分页；`offset` 从 0 起跳过已返回的条数。
    pub fn list(&self, offset: usize, limit: usize) -> AppResult<Vec<TranslationRecord>> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| AppError::Config("翻译历史存储锁已损坏".into()))?;
        let mut records = self.load_all()?;
        state.record_count = Some(records.len());
        records.sort_by(|a, b| b.created_at.cmp(&a.created_at).then_with(|| b.id.cmp(&a.id)));
        let limit = limit.max(1);
        Ok(records.into_iter().skip(offset).take(limit).collect())
    }

    /// 按 id 取单条；不存在则 `Ok(None)`。
    pub fn get(&self, id: &str) -> AppResult<Option<TranslationRecord>> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| AppError::Config("翻译历史存储锁已损坏".into()))?;
        let records = self.load_all()?;
        state.record_count = Some(records.len());
        Ok(records.into_iter().find(|record| record.id == id))
    }

    /// 清空历史文件与残留临时文件（隐私清除）。
    pub fn clear(&self) -> AppResult<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| AppError::Config("翻译历史存储锁已损坏".into()))?;
        self.clear_files()?;
        state.record_count = Some(0);
        Ok(())
    }

    fn tmp_path(&self) -> PathBuf {
        self.path.with_extension("jsonl.tmp")
    }

    fn clear_files(&self) -> AppResult<()> {
        let tmp = self.tmp_path();
        if tmp.exists() {
            fs::remove_file(&tmp)?;
        }
        if self.path.exists() {
            fs::remove_file(&self.path)?;
        }
        Ok(())
    }

    fn ensure_parent(&self) -> AppResult<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        Ok(())
    }

    fn count_records(&self) -> AppResult<usize> {
        Ok(self.load_all()?.len())
    }

    fn load_all(&self) -> AppResult<Vec<TranslationRecord>> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let file = File::open(&self.path)?;
        let reader = BufReader::new(file);
        let mut records = Vec::new();
        for (idx, line) in reader.lines().enumerate() {
            let line = line?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            match serde_json::from_str::<TranslationRecord>(trimmed) {
                Ok(record) => records.push(record),
                Err(err) => {
                    tracing::warn!(
                        line = idx + 1,
                        path = %self.path.display(),
                        error = %err,
                        "跳过无法解析的历史行"
                    );
                }
            }
        }
        Ok(records)
    }

    /// 超过上限时重写文件，仅保留最新的 `max_records` 条。
    fn trim_to_max_locked(&self, state: &mut StoreState) -> AppResult<()> {
        let mut records = self.load_all()?;
        if records.len() <= self.max_records {
            state.record_count = Some(records.len());
            return Ok(());
        }
        records.sort_by(|a, b| a.created_at.cmp(&b.created_at).then_with(|| a.id.cmp(&b.id)));
        let keep = records.split_off(records.len() - self.max_records);
        self.rewrite_all(&keep)?;
        state.record_count = Some(keep.len());
        Ok(())
    }

    fn rewrite_all(&self, records: &[TranslationRecord]) -> AppResult<()> {
        self.ensure_parent()?;
        let tmp = self.tmp_path();
        {
            let mut file = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&tmp)?;
            for record in records {
                serde_json::to_writer(&mut file, record)?;
                file.write_all(b"\n")?;
            }
            file.flush()?;
        }
        fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

/// `data_local_dir/history/translations.jsonl`
pub fn default_history_path() -> AppResult<PathBuf> {
    ProjectDirs::from("com", "chinuno", "LiveTranslate")
        .map(|dirs| {
            dirs.data_local_dir()
                .join(HISTORY_DIR_NAME)
                .join(HISTORY_FILE_NAME)
        })
        .ok_or_else(|| AppError::Config("Failed to determine history data directory".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
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
            "livetranslate-history-{}-{}-{}-{}",
            std::process::id(),
            nanos,
            n,
            tag
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create test dir");
        dir
    }

    fn record_at(id: &str, created_at: i64, source: &str) -> TranslationRecord {
        TranslationRecord {
            id: id.into(),
            created_at,
            source_id: "mic".into(),
            label: "Mic".into(),
            source: source.into(),
            translated: format!("译:{source}"),
        }
    }

    #[test]
    fn append_and_list_newest_first() {
        let dir = test_dir("list");
        let store = HistoryStore::with_path(dir.join("translations.jsonl"), 100);
        store.append(&record_at("a", 100, "one")).unwrap();
        store.append(&record_at("b", 300, "three")).unwrap();
        store.append(&record_at("c", 200, "two")).unwrap();

        let page = store.list(0, 10).unwrap();
        assert_eq!(
            page.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            vec!["b", "c", "a"]
        );
        assert_eq!(page[0].source, "three");
    }

    #[test]
    fn list_paginates_with_offset() {
        let dir = test_dir("page");
        let store = HistoryStore::with_path(dir.join("translations.jsonl"), 100);
        for i in 0..5 {
            store
                .append(&record_at(&format!("id{i}"), i as i64 * 10, &format!("s{i}")))
                .unwrap();
        }
        let page = store.list(1, 2).unwrap();
        assert_eq!(
            page.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            vec!["id3", "id2"]
        );
    }

    #[test]
    fn get_by_id_and_clear() {
        let dir = test_dir("get");
        let path = dir.join("translations.jsonl");
        let store = HistoryStore::with_path(&path, 100);
        store.append(&record_at("x", 1, "hi")).unwrap();
        assert_eq!(store.get("x").unwrap().unwrap().source, "hi");
        assert!(store.get("missing").unwrap().is_none());

        store.clear().unwrap();
        assert!(!path.exists());
        assert!(store.list(0, 10).unwrap().is_empty());
    }

    #[test]
    fn clear_also_removes_rewrite_tmp() {
        let dir = test_dir("clear-tmp");
        let path = dir.join("translations.jsonl");
        let store = HistoryStore::with_path(&path, 100);
        let tmp = path.with_extension("jsonl.tmp");
        fs::write(&tmp, b"stale").unwrap();
        store.append(&record_at("x", 1, "hi")).unwrap();
        store.clear().unwrap();
        assert!(!path.exists());
        assert!(!tmp.exists());
    }

    #[test]
    fn trim_keeps_newest_within_max() {
        let dir = test_dir("trim");
        let store = HistoryStore::with_path(dir.join("translations.jsonl"), 3);
        for i in 0..5 {
            store
                .append(&record_at(&format!("id{i}"), i as i64, &format!("s{i}")))
                .unwrap();
        }
        let all = store.list(0, 100).unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(
            all.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            vec!["id4", "id3", "id2"]
        );
    }

    #[test]
    fn default_path_lives_under_data_local_history() {
        let path = default_history_path().expect("dirs");
        assert!(path.ends_with(Path::new("history").join("translations.jsonl")));
    }

    #[test]
    fn missing_file_lists_empty_and_get_is_none() {
        let dir = test_dir("missing");
        let store = HistoryStore::with_path(dir.join("translations.jsonl"), 50);
        assert!(store.list(0, 10).unwrap().is_empty());
        assert!(store.get("nope").unwrap().is_none());
        // clear 对不存在的文件也应成功（隐私清除幂等）
        store.clear().unwrap();
    }

    #[test]
    fn skips_blank_and_malformed_jsonl_lines() {
        let dir = test_dir("corrupt");
        let path = dir.join("translations.jsonl");
        let good = record_at("ok", 50, "hello");
        let good_json = serde_json::to_string(&good).unwrap();
        fs::write(
            &path,
            format!(
                "\nnot-json\n{good_json}\n{{}}\n   \n{{\"id\":1}}\n"
            ),
        )
        .unwrap();
        let store = HistoryStore::with_path(&path, 50);
        let page = store.list(0, 10).unwrap();
        assert_eq!(page.len(), 1);
        assert_eq!(page[0].id, "ok");
        assert_eq!(store.get("ok").unwrap().unwrap().source, "hello");
    }
}

//! 翻译历史本地持久化（JSONL）。
//!
//! 路径：`ProjectDirs::data_local_dir()/history/translations.jsonl`
//! 每行一条 [`TranslationRecord`] JSON；按 `created_at` 倒序游标分页读取；
//! 超过 [`DEFAULT_MAX_RECORDS`] 时裁掉最旧记录。
//!
//! 文件是唯一真相，内存里只保留一份「已解析并按时间倒序排好」的缓存：读命令
//! 因此不再每次都全量解析 + 排序，也不会长时间占住锁把写线程顶住；追加时增量
//! 插到缓存的正确位置，超限才全量重写文件。

use super::TranslationRecord;
use crate::error::{AppError, AppResult};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

/// 默认保留上限（条）。
pub const DEFAULT_MAX_RECORDS: usize = 5000;

/// 相对 `data_local_dir` 的历史子目录名。
const HISTORY_DIR_NAME: &str = "history";
/// JSONL 文件名。
const HISTORY_FILE_NAME: &str = "translations.jsonl";

/// 分页游标：上一页最后一条的 `(created_at, id)`。
///
/// 记录按 `created_at` 倒序、同刻按 `id` 倒序排列，因此 `(created_at, id)` 唯一确定
/// 位置；用游标而不是偏移量，避免「翻页期间有新记录写入」导致重复或漏条。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryCursor {
    pub created_at: i64,
    pub id: String,
}

impl HistoryCursor {
    #[allow(dead_code)] // 单测与后续游标入口
    pub fn of(record: &TranslationRecord) -> Self {
        Self {
            created_at: record.created_at,
            id: record.id.clone(),
        }
    }
}

/// 一页历史记录。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct HistoryPage {
    pub items: Vec<TranslationRecord>,
    /// 是否还有更早的记录（由存储层判定，前端不必用「本页是否满」猜）。
    pub has_more: bool,
}

#[derive(Debug, Default)]
struct StoreState {
    /// 已解析并按 `created_at`/`id` 倒序排好的记录缓存；`None` 表示需要从磁盘重建。
    cached: Option<Vec<TranslationRecord>>,
}

/// 本地 JSONL 历史存储。
///
/// 通过内部 `Mutex` 串行化同一路径上的读写 / 清空；临界区只做内存操作或单次追加
/// （解析缓存命中后不再整文件扫描），避免读命令把写线程堵在队列里。
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

    /// 追加一条记录；仅在缓存条数超过 `max_records` 时全量裁剪重写。
    pub fn append(&self, record: &TranslationRecord) -> AppResult<()> {
        let mut state = self.lock()?;
        // 先建缓存：失败时直接放弃本次写入，不留下无法计入上限的记录
        self.ensure_cache(&mut state)?;
        self.ensure_parent()?;
        // 整行一次写入：崩溃时最多丢掉这一行，不会与下一条粘连成坏行
        let mut line = serde_json::to_vec(record)?;
        line.push(b'\n');
        {
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)?;
            file.write_all(&line)?;
            file.flush()?;
        }
        let over_limit = match state.cached.as_mut() {
            Some(cached) => {
                insert_cached(cached, record.clone());
                cached.len() > self.max_records
            }
            None => false,
        };
        if over_limit {
            self.trim_locked(&mut state)?;
        }
        Ok(())
    }

    /// 游标分页：返回严格早于 `before` 的一页（`None` 表示第一页）。
    pub fn list_page(
        &self,
        before: Option<&HistoryCursor>,
        limit: usize,
    ) -> AppResult<HistoryPage> {
        let mut state = self.lock()?;
        self.ensure_cache(&mut state)?;
        let records = state.cached.as_deref().unwrap_or_default();
        // 缓存按 `(created_at, id)` 倒序，因此「早于游标」构成前缀，可二分定位
        let start = match before {
            Some(cursor) => records.partition_point(|record| {
                (record.created_at, record.id.as_str()) >= (cursor.created_at, cursor.id.as_str())
            }),
            None => 0,
        };
        let limit = limit.max(1);
        let end = start.saturating_add(limit).min(records.len());
        Ok(HistoryPage {
            items: records[start..end].to_vec(),
            has_more: end < records.len(),
        })
    }

    /// 按 id 取单条；不存在则 `Ok(None)`。
    pub fn get(&self, id: &str) -> AppResult<Option<TranslationRecord>> {
        let mut state = self.lock()?;
        self.ensure_cache(&mut state)?;
        Ok(state
            .cached
            .as_deref()
            .unwrap_or_default()
            .iter()
            .find(|record| record.id == id)
            .cloned())
    }

    /// 清空历史文件与残留临时文件（隐私清除）。
    pub fn clear(&self) -> AppResult<()> {
        let mut state = self.lock()?;
        self.clear_files()?;
        state.cached = Some(Vec::new());
        Ok(())
    }

    fn lock(&self) -> AppResult<MutexGuard<'_, StoreState>> {
        self.state
            .lock()
            .map_err(|_| AppError::Config("翻译历史存储锁已损坏".into()))
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

    /// 缓存未建立时从磁盘重建（只在进程内首次访问 / 清空后发生一次）。
    fn ensure_cache(&self, state: &mut StoreState) -> AppResult<()> {
        if state.cached.is_none() {
            let mut records = self.load_all()?;
            sort_newest_first(&mut records);
            state.cached = Some(records);
        }
        Ok(())
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

    /// 超过上限时用缓存重写文件，仅保留最新的 `max_records` 条。
    fn trim_locked(&self, state: &mut StoreState) -> AppResult<()> {
        let Some(mut records) = state.cached.take() else {
            return Ok(());
        };
        if records.len() > self.max_records {
            records.truncate(self.max_records);
            let result = self.rewrite_all(&records);
            // 无论重写成功与否都放回缓存：失败时下次读取会从磁盘重建
            state.cached = Some(records);
            return result;
        }
        state.cached = Some(records);
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

/// 按 `created_at` 倒序；同刻按 `id` 倒序，保证分页顺序稳定。
fn sort_newest_first(records: &mut [TranslationRecord]) {
    records.sort_by(|a, b| {
        b.created_at
            .cmp(&a.created_at)
            .then_with(|| b.id.cmp(&a.id))
    });
}

/// 把新记录插到缓存的正确位置（保持倒序），避免每条追加都整体重排。
fn insert_cached(cached: &mut Vec<TranslationRecord>, record: TranslationRecord) {
    let index = cached.partition_point(|item| {
        (item.created_at, item.id.as_str()) > (record.created_at, record.id.as_str())
    });
    cached.insert(index, record);
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

    fn ids(page: &HistoryPage) -> Vec<&str> {
        page.items.iter().map(|r| r.id.as_str()).collect()
    }

    #[test]
    fn append_and_list_newest_first() {
        let dir = test_dir("list");
        let store = HistoryStore::with_path(dir.join("translations.jsonl"), 100);
        store.append(&record_at("a", 100, "one")).unwrap();
        store.append(&record_at("b", 300, "three")).unwrap();
        store.append(&record_at("c", 200, "two")).unwrap();

        let page = store.list_page(None, 10).unwrap();
        assert_eq!(ids(&page), vec!["b", "c", "a"]);
        assert_eq!(page.items[0].source, "three");
        assert!(!page.has_more);
    }

    /// 追加时增量插入缓存也必须保持倒序（乱序写入 created_at）
    #[test]
    fn incremental_cache_keeps_descending_order() {
        let dir = test_dir("order");
        let store = HistoryStore::with_path(dir.join("translations.jsonl"), 100);
        for (id, created_at) in [("a", 100), ("c", 300), ("b", 200), ("d", 400), ("e", 50)] {
            store.append(&record_at(id, created_at, id)).unwrap();
        }
        assert_eq!(
            ids(&store.list_page(None, 100).unwrap()),
            vec!["d", "c", "b", "a", "e"]
        );
        // 同样时刻的记录按 id 倒序
        store.append(&record_at("z", 400, "z")).unwrap();
        assert_eq!(ids(&store.list_page(None, 2).unwrap()), vec!["z", "d"]);
    }

    #[test]
    fn cursor_pagination_is_stable_across_new_appends() {
        let dir = test_dir("cursor");
        let store = HistoryStore::with_path(dir.join("translations.jsonl"), 100);
        for i in 0..5 {
            store
                .append(&record_at(
                    &format!("id{i}"),
                    i as i64 * 10,
                    &format!("s{i}"),
                ))
                .unwrap();
        }

        let first = store.list_page(None, 2).unwrap();
        assert_eq!(ids(&first), vec!["id4", "id3"]);
        assert!(first.has_more);

        // 翻页期间来了一条新记录：游标分页不应重复或漏掉旧记录
        store.append(&record_at("id5", 50, "s5")).unwrap();
        let cursor = HistoryCursor::of(&first.items[1]);
        let second = store.list_page(Some(&cursor), 2).unwrap();
        assert_eq!(ids(&second), vec!["id2", "id1"]);

        let cursor = HistoryCursor::of(&second.items[1]);
        let third = store.list_page(Some(&cursor), 2).unwrap();
        assert_eq!(ids(&third), vec!["id0"]);
        assert!(!third.has_more);

        let cursor = HistoryCursor::of(&third.items[0]);
        assert!(store.list_page(Some(&cursor), 2).unwrap().items.is_empty());
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
        assert!(store.list_page(None, 10).unwrap().items.is_empty());
        // 清空后仍可继续写入，且不影响上限维护
        store.append(&record_at("y", 2, "again")).unwrap();
        assert_eq!(ids(&store.list_page(None, 10).unwrap()), vec!["y"]);
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
        let all = store.list_page(None, 100).unwrap();
        assert_eq!(ids(&all), vec!["id4", "id3", "id2"]);

        // 重写后的文件本身也只保留 3 条
        let reloaded = HistoryStore::with_path(store.path(), 3);
        assert_eq!(
            ids(&reloaded.list_page(None, 100).unwrap()),
            vec!["id4", "id3", "id2"]
        );
    }

    /// 重启后（缓存为空）首次追加也要按磁盘上已有条数裁剪，不能无限增长
    #[test]
    fn first_append_after_restart_trims_existing_overflow() {
        let dir = test_dir("restart");
        let path = dir.join("translations.jsonl");
        {
            let store = HistoryStore::with_path(&path, 3);
            for i in 0..3 {
                store
                    .append(&record_at(&format!("old{i}"), i as i64, "s"))
                    .unwrap();
            }
        }
        let reopened = HistoryStore::with_path(&path, 3);
        reopened.append(&record_at("new", 100, "n")).unwrap();
        let page = reopened.list_page(None, 100).unwrap();
        assert_eq!(ids(&page), vec!["new", "old2", "old1"]);
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
        assert!(store.list_page(None, 10).unwrap().items.is_empty());
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
            format!("\nnot-json\n{good_json}\n{{}}\n   \n{{\"id\":1}}\n"),
        )
        .unwrap();
        let store = HistoryStore::with_path(&path, 50);
        let page = store.list_page(None, 10).unwrap();
        assert_eq!(ids(&page), vec!["ok"]);
        assert_eq!(store.get("ok").unwrap().unwrap().source, "hello");
    }
}

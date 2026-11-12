//! 翻译历史本地持久化（分段 JSONL）。
//!
//! 路径：`ProjectDirs::data_local_dir()/history/`，目录内 `seg-*.jsonl`。
//! 每行一条 [`TranslationRecord`] JSON；按 `seq` 倒序游标分页读取。
//! 保留窗口按段数裁剪（`unlink` 最旧段），写路径恒为 O(1) 追加。

use super::TranslationRecord;
use crate::error::{AppError, AppResult};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// 默认保留上限（条）。
pub const DEFAULT_MAX_RECORDS: usize = 5000;

/// `seq = (ordinal << SEQ_INDEX_BITS) | index_in_segment`；读写两侧共用。
pub const SEQ_INDEX_BITS: u32 = 20;

const SEQ_INDEX_MASK: u64 = (1u64 << SEQ_INDEX_BITS) - 1;

/// 相对 `data_local_dir` 的历史子目录名。
const HISTORY_DIR_NAME: &str = "history";
/// 遗留单文件名（启动时删除，不再读取）。
const LEGACY_FILE_NAME: &str = "translations.jsonl";
/// 单段条数硬上限因子；实际 `segment_records = min(SEGMENT_RECORDS_CAP, max_records)`。
const SEGMENT_RECORDS_CAP: usize = 500;
/// `seq` 打包能容纳的段内 index 数；`make_seq` / `open_state` 依赖这个前提。
const SEG_INDEX_LIMIT: usize = 1 << SEQ_INDEX_BITS;
const _: () = assert!(SEGMENT_RECORDS_CAP <= SEG_INDEX_LIMIT);
/// LRU 最多缓存的完整段数。
const LRU_CAPACITY: usize = 3;
/// 旋转时 `create_new` 冲突重试次数。
const ROTATE_CREATE_RETRIES: usize = 3;
/// 段号 6 位上限。
const MAX_ORDINAL: u32 = 999_999;

/// 分页游标：上一页最后一条的 `seq`。
///
/// 记录按 `seq` 倒序排列，因此 `seq` 唯一确定位置；用游标而不是偏移量，避免
/// 「翻页期间有新记录写入」导致重复或漏条。游标严格排他（不含游标自身）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryCursor {
    pub seq: u64,
}

impl HistoryCursor {
    #[allow(dead_code)] // 单测与后续游标入口
    pub fn of(record: &TranslationRecord) -> Self {
        Self { seq: record.seq }
    }
}

/// 一页历史记录。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct HistoryPage {
    pub items: Vec<TranslationRecord>,
    /// 是否还有更早的记录（由存储层判定，前端不必用「本页是否满」猜）。
    pub has_more: bool,
}

struct ActiveSegment {
    file: File,
    ordinal: u32,
    next_index: u32,
}

/// ordinal → (records, complete)。`complete=false` 等同未命中，必须整段重解析。
struct SegmentCache {
    /// MRU 在前端；最多 [`LRU_CAPACITY`] 条。
    entries: VecDeque<(u32, Arc<Vec<TranslationRecord>>, bool)>,
}

impl SegmentCache {
    fn new() -> Self {
        Self {
            entries: VecDeque::new(),
        }
    }

    fn clear(&mut self) {
        self.entries.clear();
    }

    fn remove(&mut self, ord: u32) {
        self.entries.retain(|(o, _, _)| *o != ord);
    }

    fn get_complete(&mut self, ord: u32) -> Option<Arc<Vec<TranslationRecord>>> {
        let idx = self
            .entries
            .iter()
            .position(|(o, _, complete)| *o == ord && *complete)?;
        let entry = self.entries.remove(idx).expect("index from position");
        let records = Arc::clone(&entry.1);
        self.entries.push_front(entry);
        Some(records)
    }

    fn insert_complete(&mut self, ord: u32, records: Arc<Vec<TranslationRecord>>) {
        self.entries.retain(|(o, _, _)| *o != ord);
        self.entries.push_front((ord, records, true));
        while self.entries.len() > LRU_CAPACITY {
            self.entries.pop_back();
        }
    }
}

struct SegState {
    dir: PathBuf,
    segment_records: usize,
    max_segments: usize,
    /// 升序；可有空洞；最后一个是活动段（若存在）。
    segments: Vec<u32>,
    /// 下一个可用新段号；`clear` 不重置。
    next_ordinal: u32,
    active: Option<ActiveSegment>,
    lru: SegmentCache,
}

/// 本地分段 JSONL 历史存储。
///
/// 通过内部 `Mutex` 串行化同一目录上的读写 / 清空；写线程独占活动段句柄。
#[derive(Clone)]
pub struct HistoryStore {
    dir: PathBuf,
    max_records: usize,
    /// 惰性打开的分段状态；打开失败不缓存，下次访问自动重试。
    state: Arc<Mutex<StoreState>>,
}

/// 存储内部状态：`seg = None` 表示尚未打开（或上次打开失败，需要重试）。
#[derive(Default)]
struct StoreState {
    seg: Option<SegState>,
}

impl StoreState {
    /// 惰性打开；失败不缓存错误，下次访问重试，让瞬时故障（fd 用尽、卷临时
    /// 不可用、目录刚被建好）能自愈，而不是整个会话永久禁用历史。
    fn ensure(&mut self, dir: &Path, max_records: usize) -> AppResult<&mut SegState> {
        if self.seg.is_none() {
            self.seg = Some(open_state(dir, max_records)?);
        }
        Ok(self.seg.as_mut().expect("上面已保证 Some"))
    }
}

impl std::fmt::Debug for HistoryStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HistoryStore")
            .field("dir", &self.dir)
            .field("max_records", &self.max_records)
            .finish_non_exhaustive()
    }
}

impl HistoryStore {
    /// 使用默认目录与保留上限；`open` 失败则禁用持久化。
    ///
    /// 与 [`Self::with_dir`] 不同，这里把启动阶段的打开失败**上报**，让调用方
    /// 看得见并降级（`OptionalHistoryWriter::try_spawn_default_with_store`）。
    pub fn open_default() -> AppResult<Self> {
        let dir = default_history_dir()?;
        let max_records = DEFAULT_MAX_RECORDS;
        let mut state = StoreState::default();
        state.ensure(&dir, max_records)?;
        Ok(Self {
            dir,
            max_records,
            state: Arc::new(Mutex::new(state)),
        })
    }

    /// 指定历史目录（测试或自定义）；`max_records` 至少为 1。
    ///
    /// 构造时先试一次（目录创建 / 遗留文件清理 / 超限裁剪立即生效）；失败不报错，
    /// 留给后续 `append` / `list_page` / `get` / `clear` 重试（见 [`StoreState::ensure`]）。
    #[allow(dead_code)] // 单测与后续自定义路径入口
    pub fn with_dir(dir: impl Into<PathBuf>, max_records: usize) -> Self {
        let dir = dir.into();
        let max_records = max_records.max(1);
        let mut state = StoreState::default();
        if let Err(err) = state.ensure(&dir, max_records) {
            tracing::warn!(
                error = %err,
                path = %dir.display(),
                "打开翻译历史存储失败，将在下次访问重试"
            );
        }
        Self {
            dir,
            max_records,
            state: Arc::new(Mutex::new(state)),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn max_records(&self) -> usize {
        self.max_records
    }

    /// 追加一条记录并分配 `seq`；段满时旋转，超段数时 unlink 最旧段。
    pub fn append(&self, record: TranslationRecord) -> AppResult<TranslationRecord> {
        self.with_seg(|seg| seg.append(record))
    }

    /// 游标分页：返回严格早于 `before`（`seq` 更小）的一页（`None` 表示第一页）。
    pub fn list_page(
        &self,
        before: Option<&HistoryCursor>,
        limit: usize,
    ) -> AppResult<HistoryPage> {
        self.with_seg(|seg| seg.list_page(before, limit))
    }

    /// 按 `seq` 取单条；不存在则 `Ok(None)`。
    pub fn get(&self, seq: u64) -> AppResult<Option<TranslationRecord>> {
        self.with_seg(|seg| seg.get(seq))
    }

    /// 清空全部段文件与遗留文件；保留 `next_ordinal`。
    pub fn clear(&self) -> AppResult<()> {
        self.with_seg(SegState::clear)
    }

    /// 在持锁且状态已打开的前提下执行 `f`（唯一的加锁入口）。
    fn with_seg<R>(&self, f: impl FnOnce(&mut SegState) -> AppResult<R>) -> AppResult<R> {
        let mut guard = self
            .state
            .lock()
            .map_err(|_| AppError::Config("翻译历史存储锁已损坏".into()))?;
        f(guard.ensure(&self.dir, self.max_records)?)
    }
}

impl SegState {
    fn append(&mut self, record: TranslationRecord) -> AppResult<TranslationRecord> {
        if self
            .active
            .as_ref()
            .is_none_or(|a| a.next_index as usize >= self.segment_records)
        {
            self.rotate()?;
        }
        let active = self.active.as_mut().expect("rotate 已保证 active 存在");
        let stored = TranslationRecord {
            seq: make_seq(active.ordinal, active.next_index),
            ..record
        };

        let len_before = active.file.metadata().map(|m| m.len()).unwrap_or(0);
        if let Err(err) = write_line(&mut active.file, &stored) {
            if let Err(rollback) = active.file.set_len(len_before) {
                tracing::warn!(
                    error = %rollback,
                    path = %segment_path(&self.dir, active.ordinal).display(),
                    "写历史失败后回滚段文件长度也失败"
                );
            }
            return Err(err);
        }
        active.next_index += 1;
        let ord = active.ordinal;
        self.lru_push(ord, stored.clone());
        if self.segments.len() > self.max_segments {
            self.drop_oldest();
        }
        Ok(stored)
    }

    fn list_page(
        &mut self,
        before: Option<&HistoryCursor>,
        limit: usize,
    ) -> AppResult<HistoryPage> {
        let limit = limit.max(1);
        let Some(newest) = self.newest_ordinal() else {
            return Ok(HistoryPage::default());
        };
        let oldest = self.oldest_ordinal().expect("有 newest 必有 oldest");
        let start = match before {
            Some(cursor) => {
                let ord = ordinal_of(cursor.seq);
                if ord > newest || ord < oldest {
                    return Ok(HistoryPage::default());
                }
                ord
            }
            None => newest,
        };
        // `segments` 升序且允许空洞，直接按实际存在的段倒序走；不能用段号数值递减，
        // 否则大空洞会对每个不存在的段号做一次 File::open（持锁 + 污染 LRU）。
        let mut ords: Vec<u32> = self
            .segments
            .iter()
            .copied()
            .filter(|ord| *ord <= start)
            .collect();
        ords.reverse();

        let mut upper = before.map(|c| c.seq);
        let mut out = Vec::with_capacity(limit);

        for (idx, &ord) in ords.iter().enumerate() {
            let records = self.segment(ord);
            let end = match upper {
                Some(seq) if ordinal_of(seq) == ord => records.partition_point(|r| r.seq < seq),
                _ => records.len(),
            };
            upper = None;
            let mut i = end;
            while i > 0 {
                if out.len() == limit {
                    // i > 0：本段还有更早的记录
                    return Ok(HistoryPage {
                        items: out,
                        has_more: true,
                    });
                }
                i -= 1;
                out.push(records[i].clone());
            }
            if out.len() == limit {
                // 本段恰好读满：has_more 取决于更旧段里是否还有可解析记录
                let has_more = ords[idx + 1..]
                    .iter()
                    .any(|ord| !self.segment(*ord).is_empty());
                return Ok(HistoryPage {
                    items: out,
                    has_more,
                });
            }
        }
        Ok(HistoryPage {
            items: out,
            has_more: false,
        })
    }

    fn get(&mut self, seq: u64) -> AppResult<Option<TranslationRecord>> {
        let Some(newest) = self.newest_ordinal() else {
            return Ok(None);
        };
        let oldest = self.oldest_ordinal().expect("有 newest 必有 oldest");
        let ord = ordinal_of(seq);
        if ord > newest || ord < oldest {
            return Ok(None);
        }
        let records = self.segment(ord);
        Ok(records
            .binary_search_by_key(&seq, |r| r.seq)
            .ok()
            .map(|i| records[i].clone()))
    }

    fn clear(&mut self) -> AppResult<()> {
        self.active = None;
        self.lru.clear();
        let mut first_err: Option<AppError> = None;

        for ord in std::mem::take(&mut self.segments) {
            let path = segment_path(&self.dir, ord);
            if let Err(err) = remove_file_retry(&path) {
                tracing::warn!(error = %err, path = %path.display(), "清空历史时删除段失败");
                if first_err.is_none() {
                    first_err = Some(err);
                }
            }
        }
        for name in [LEGACY_FILE_NAME, "translations.jsonl.tmp"] {
            let path = self.dir.join(name);
            if let Err(err) = remove_file_retry(&path) {
                if path.exists() {
                    tracing::warn!(error = %err, path = %path.display(), "清空历史时删除遗留文件失败");
                    if first_err.is_none() {
                        first_err = Some(err);
                    }
                }
            }
        }
        // 再扫一遍，防止遗漏
        if let Ok(entries) = fs::read_dir(&self.dir) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let lossy = name.to_string_lossy();
                if parse_segment_name(&lossy).is_some() || is_legacy_name(&lossy) {
                    let path = entry.path();
                    if let Err(err) = remove_file_retry(&path) {
                        tracing::warn!(error = %err, path = %path.display(), "清空历史时删除残留失败");
                        if first_err.is_none() {
                            first_err = Some(err);
                        }
                    }
                }
            }
        }
        // next_ordinal 保留
        match first_err {
            Some(err) if segment_files_remain(&self.dir) => Err(err),
            _ => Ok(()),
        }
    }

    fn rotate(&mut self) -> AppResult<()> {
        if let Some(mut active) = self.active.take() {
            if let Err(err) = active.file.flush() {
                tracing::warn!(error = %err, "关闭活动历史段时 flush 失败");
                return Err(err.into());
            }
        }
        for _ in 0..ROTATE_CREATE_RETRIES {
            if self.next_ordinal > MAX_ORDINAL {
                tracing::warn!(
                    next_ordinal = self.next_ordinal,
                    "历史段号已达上限，停止写入"
                );
                return Err(AppError::Config("翻译历史段号已达上限".into()));
            }
            let ord = self.next_ordinal;
            let path = segment_path(&self.dir, ord);
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(file) => {
                    self.next_ordinal = self.next_ordinal.saturating_add(1);
                    if self.segments.last().copied() != Some(ord) {
                        self.segments.push(ord);
                        self.segments.sort_unstable();
                        self.segments.dedup();
                    }
                    self.active = Some(ActiveSegment {
                        file,
                        ordinal: ord,
                        next_index: 0,
                    });
                    #[cfg(unix)]
                    {
                        if let Ok(dir_file) = File::open(&self.dir) {
                            let _ = dir_file.sync_all();
                        }
                    }
                    return Ok(());
                }
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                    self.next_ordinal = self.next_ordinal.saturating_add(1);
                    continue;
                }
                Err(err) => {
                    tracing::warn!(
                        error = %err,
                        path = %path.display(),
                        "创建新历史段失败"
                    );
                    return Err(err.into());
                }
            }
        }
        tracing::warn!("创建新历史段连续冲突，丢弃本条");
        Err(AppError::Config("翻译历史段创建冲突".into()))
    }

    fn drop_oldest(&mut self) {
        let Some(&ord) = self.segments.first() else {
            return;
        };
        // 不应删除仍持有句柄的活动段
        if self.active.as_ref().map(|a| a.ordinal) == Some(ord) {
            return;
        }
        let path = segment_path(&self.dir, ord);
        if let Err(err) = remove_file_retry(&path) {
            tracing::warn!(
                error = %err,
                path = %path.display(),
                "删除最旧历史段失败，下次 append 再试"
            );
            return;
        }
        self.segments.remove(0);
        self.lru.remove(ord);
    }

    fn newest_ordinal(&self) -> Option<u32> {
        self.segments.last().copied()
    }

    fn oldest_ordinal(&self) -> Option<u32> {
        self.segments.first().copied()
    }

    fn segment(&mut self, ord: u32) -> Arc<Vec<TranslationRecord>> {
        if let Some(records) = self.lru.get_complete(ord) {
            return records;
        }
        let records = Arc::new(load_segment_file(&segment_path(&self.dir, ord)));
        self.lru.insert_complete(ord, Arc::clone(&records));
        records
    }

    fn lru_push(&mut self, ord: u32, record: TranslationRecord) {
        if let Some(cached) = self.lru.get_complete(ord) {
            let mut next = (*cached).clone();
            next.push(record);
            self.lru.insert_complete(ord, Arc::new(next));
            return;
        }
        let mut records = load_segment_file(&segment_path(&self.dir, ord));
        if records.last().map(|r| r.seq) != Some(record.seq) {
            records.push(record);
        }
        self.lru.insert_complete(ord, Arc::new(records));
    }
}

fn open_state(dir: &Path, max_records: usize) -> AppResult<SegState> {
    let max_records = max_records.max(1);
    let segment_records = SEGMENT_RECORDS_CAP.min(max_records);
    let max_segments = max_segments_for(max_records, segment_records);

    fs::create_dir_all(dir)?;
    remove_legacy_files(dir);

    let mut segments = scan_segments(dir);
    let next_ordinal = segments.last().map_or(0, |o| o.saturating_add(1));

    while segments.len() > max_segments {
        let oldest = segments.remove(0);
        let path = segment_path(dir, oldest);
        if let Err(err) = remove_file_retry(&path) {
            tracing::warn!(
                error = %err,
                path = %path.display(),
                "启动时裁剪超限历史段失败"
            );
            // 裁不掉就先留下，避免 open 整体失败；segments 仍按已删意图移除会导致幽灵
            // 这里删失败则把 ordinal 放回并停止，避免状态与磁盘脱节
            segments.insert(0, oldest);
            break;
        }
    }

    let mut active = None;
    if let Some(&ord) = segments.last() {
        let path = segment_path(dir, ord);
        // 崩溃可能留下半行（没有结尾 `\n`）：先截掉，否则下一条记录会粘在残行尾部
        // 合成一条不可解析的坏行 → 重启后的第一条记录静默丢失。
        repair_trailing_partial_line(&path);
        let records = load_segment_file(&path);
        let next_index = records
            .iter()
            .map(|r| index_of(r.seq))
            .max()
            .map_or(0, |i| i.saturating_add(1))
            // 段内 index 溢出到段号位（段文件被污染 / max_records 调小）时按满段处理，
            // 让下一条 append 直接旋转到新段，保证 seq 不重复。
            .min(segment_records as u32);
        match OpenOptions::new().append(true).open(&path) {
            Ok(file) => {
                active = Some(ActiveSegment {
                    file,
                    ordinal: ord,
                    next_index,
                });
            }
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    path = %path.display(),
                    "打开活动历史段失败"
                );
                return Err(err.into());
            }
        }
    }

    Ok(SegState {
        dir: dir.to_path_buf(),
        segment_records,
        max_segments,
        segments,
        next_ordinal,
        active,
        lru: SegmentCache::new(),
    })
}

fn max_segments_for(max_records: usize, segment_records: usize) -> usize {
    let segment_records = segment_records.max(1);
    max_records.div_ceil(segment_records).max(1)
}

/// 截掉文件末尾不完整的最后一行（崩溃留下的半行），返回是否截断。
///
/// `write_line` 只 `flush` 不 `fsync`，进程被杀 / 断电可能留下半行；若不解处理，
/// 下一次 append（`O_APPEND`）会把它与后续记录连成一条无法解析的行。
fn repair_trailing_partial_line(path: &Path) -> bool {
    let len = match fs::metadata(path) {
        Ok(meta) => meta.len(),
        Err(_) => return false,
    };
    if len == 0 {
        return false;
    }
    let body = match fs::read(path) {
        Ok(body) => body,
        Err(err) => {
            tracing::warn!(
                error = %err,
                path = %path.display(),
                "读取历史段失败，跳过残行修复"
            );
            return false;
        }
    };
    if body.last() == Some(&b'\n') {
        return false;
    }
    let keep = body
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |idx| idx + 1);
    let dropped = body.len() - keep;
    match OpenOptions::new().write(true).open(path) {
        Ok(file) => match file.set_len(keep as u64) {
            Ok(()) => {
                tracing::warn!(
                    path = %path.display(),
                    dropped,
                    "历史段末行不完整（上次写入未完成），已截断修复"
                );
                true
            }
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    path = %path.display(),
                    "截断历史段残行失败"
                );
                false
            }
        },
        Err(err) => {
            tracing::warn!(
                error = %err,
                path = %path.display(),
                "打开历史段失败，跳过残行修复"
            );
            false
        }
    }
}

fn remove_legacy_files(dir: &Path) {
    for name in [LEGACY_FILE_NAME, "translations.jsonl.tmp"] {
        let path = dir.join(name);
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    path = %path.display(),
                    "删除遗留翻译历史文件失败"
                );
            }
        }
    }
}

fn scan_segments(dir: &Path) -> Vec<u32> {
    let mut ords = Vec::new();
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(err) => {
            tracing::warn!(error = %err, path = %dir.display(), "扫描历史段目录失败");
            return ords;
        }
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if let Some(ord) = parse_segment_name(&name.to_string_lossy()) {
            ords.push(ord);
        }
    }
    ords.sort_unstable();
    ords.dedup();
    ords
}

fn parse_segment_name(name: &str) -> Option<u32> {
    let rest = name.strip_prefix("seg-")?.strip_suffix(".jsonl")?;
    if rest.is_empty() || !rest.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    rest.parse().ok()
}

fn is_legacy_name(name: &str) -> bool {
    name == LEGACY_FILE_NAME || name == "translations.jsonl.tmp"
}

fn segment_path(dir: &Path, ordinal: u32) -> PathBuf {
    dir.join(format!("seg-{ordinal:06}.jsonl"))
}

fn segment_files_remain(dir: &Path) -> bool {
    scan_segments(dir)
        .into_iter()
        .any(|ord| segment_path(dir, ord).exists())
}

fn remove_file_retry(path: &Path) -> AppResult<()> {
    let mut last_err = None;
    for attempt in 0..3 {
        match fs::remove_file(path) {
            Ok(()) => return Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(err) => {
                last_err = Some(err);
                if attempt + 1 < 3 {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
            }
        }
    }
    Err(last_err
        .map(AppError::from)
        .unwrap_or_else(|| AppError::Config("删除历史文件失败".into())))
}

fn load_segment_file(path: &Path) -> Vec<TranslationRecord> {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(err) => {
            if err.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(
                    error = %err,
                    path = %path.display(),
                    "读取历史段失败，按空段处理"
                );
            }
            return Vec::new();
        }
    };
    let reader = BufReader::new(file);
    let mut records = Vec::new();
    for (idx, line) in reader.lines().enumerate() {
        let line = match line {
            Ok(l) => l,
            Err(err) => {
                tracing::warn!(
                    line = idx + 1,
                    path = %path.display(),
                    error = %err,
                    "跳过无法读取的历史行"
                );
                continue;
            }
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<TranslationRecord>(trimmed) {
            Ok(record) => records.push(record),
            Err(err) => {
                tracing::warn!(
                    line = idx + 1,
                    path = %path.display(),
                    error = %err,
                    "跳过无法解析的历史行"
                );
            }
        }
    }
    records.sort_by_key(|a| a.seq);
    records
}

fn write_line(file: &mut File, record: &TranslationRecord) -> AppResult<()> {
    let mut line = serde_json::to_vec(record)?;
    line.push(b'\n');
    file.write_all(&line)?;
    file.flush()?;
    Ok(())
}

#[inline]
fn make_seq(ordinal: u32, index: u32) -> u64 {
    debug_assert!(
        u64::from(index) < (1u64 << SEQ_INDEX_BITS),
        "段内 index 溢出：{index} 会污染段号位"
    );
    ((ordinal as u64) << SEQ_INDEX_BITS) | u64::from(index)
}

#[inline]
fn ordinal_of(seq: u64) -> u32 {
    (seq >> SEQ_INDEX_BITS) as u32
}

#[inline]
fn index_of(seq: u64) -> u32 {
    (seq & SEQ_INDEX_MASK) as u32
}

/// `data_local_dir/history/`
pub fn default_history_dir() -> AppResult<PathBuf> {
    ProjectDirs::from("com", "chinuno", "LiveTranslate")
        .map(|dirs| dirs.data_local_dir().join(HISTORY_DIR_NAME))
        .ok_or_else(|| AppError::Config("Failed to determine history data directory".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;

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
            seq: 0,
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

    fn list_seg_names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("seg-") && n.ends_with(".jsonl"))
            .collect();
        names.sort();
        names
    }

    fn file_fingerprint(path: &Path) -> (u64, u64) {
        let bytes = fs::read(path).unwrap_or_default();
        let mut hasher = DefaultHasher::new();
        bytes.hash(&mut hasher);
        (hasher.finish(), bytes.len() as u64)
    }

    #[test]
    fn append_and_list_newest_first() {
        let dir = test_dir("list");
        let store = HistoryStore::with_dir(&dir, 100);
        store.append(record_at("a", 100, "one")).unwrap();
        store.append(record_at("b", 300, "three")).unwrap();
        store.append(record_at("c", 200, "two")).unwrap();

        // 排序键是 seq（追加序），与 created_at 无关
        let page = store.list_page(None, 10).unwrap();
        assert_eq!(ids(&page), vec!["c", "b", "a"]);
        assert_eq!(page.items[0].source, "two");
        assert_eq!(page.items[0].seq, make_seq(0, 2));
        assert!(!page.has_more);
    }

    /// 乱序 created_at 不影响按 seq 倒序
    #[test]
    fn out_of_order_created_at_does_not_break_paging() {
        let dir = test_dir("order");
        let store = HistoryStore::with_dir(&dir, 100);
        for (id, created_at) in [("a", 100), ("c", 300), ("b", 200), ("d", 400), ("e", 50)] {
            store.append(record_at(id, created_at, id)).unwrap();
        }
        assert_eq!(
            ids(&store.list_page(None, 100).unwrap()),
            vec!["e", "d", "b", "c", "a"]
        );
        store.append(record_at("z", 400, "z")).unwrap();
        assert_eq!(ids(&store.list_page(None, 2).unwrap()), vec!["z", "e"]);
    }

    /// §10 测试 6：翻页期间旋转 + 新写入，不重复不丢条。
    #[test]
    fn cursor_pagination_is_stable_across_rotation() {
        let dir = test_dir("cursor-rotate");
        let store = HistoryStore::with_dir(&dir, 501);
        for i in 0..500 {
            store
                .append(record_at(&format!("id{i}"), i as i64, &format!("s{i}")))
                .unwrap();
        }

        let first = store.list_page(None, 2).unwrap();
        assert_eq!(ids(&first), vec!["id499", "id498"]);
        assert!(first.has_more);

        // 翻页期间旋转到 seg-1 并写入新记录：旧游标页不应重复/漏条，也不应夹带新段
        for i in 500..505 {
            store
                .append(record_at(&format!("id{i}"), i as i64, &format!("s{i}")))
                .unwrap();
        }
        assert!(dir.join("seg-000001.jsonl").exists());

        let cursor = HistoryCursor::of(&first.items[1]);
        let second = store.list_page(Some(&cursor), 2).unwrap();
        assert_eq!(ids(&second), vec!["id497", "id496"]);
        assert!(!ids(&second).contains(&"id498"));
        assert!(!ids(&second).iter().any(|id| id.starts_with("id50")));

        let cursor = HistoryCursor::of(&second.items[1]);
        let third = store.list_page(Some(&cursor), 2).unwrap();
        assert_eq!(ids(&third), vec!["id495", "id494"]);
    }

    /// §10 测试 5：首页最后一条不得出现在第二页。
    #[test]
    fn cursor_is_exclusive_regression() {
        let dir = test_dir("cursor-exclusive");
        let store = HistoryStore::with_dir(&dir, 100);
        for i in 0..5 {
            store
                .append(record_at(&format!("id{i}"), i as i64, &format!("s{i}")))
                .unwrap();
        }
        let first = store.list_page(None, 2).unwrap();
        assert_eq!(ids(&first), vec!["id4", "id3"]);
        let cursor = HistoryCursor::of(&first.items[1]);
        let second = store.list_page(Some(&cursor), 2).unwrap();
        assert_eq!(ids(&second), vec!["id2", "id1"]);
        assert!(!ids(&second).contains(&"id3"));
        assert!(!ids(&second).contains(&"id4"));
    }

    /// §10 测试 4：最新段不足一页时跨段续读。
    #[test]
    fn list_page_spans_segments() {
        let dir = test_dir("span");
        let store = HistoryStore::with_dir(&dir, 501);
        for i in 0..503 {
            store
                .append(record_at(&format!("id{i}"), i as i64, "s"))
                .unwrap();
        }
        // seg-0: 500 条，seg-1: 3 条
        let page = store.list_page(None, 50).unwrap();
        assert_eq!(page.items.len(), 50);
        assert_eq!(ids(&page)[..3], ["id502", "id501", "id500"]);
        assert_eq!(page.items[3].id, "id499");
        assert!(page.has_more);
        assert_eq!(page.items[0].seq, make_seq(1, 2));
        assert_eq!(page.items[3].seq, make_seq(0, 499));
    }

    /// §10 测试 7：高位游标 → 空页。
    #[test]
    fn cursor_beyond_newest_returns_empty() {
        let dir = test_dir("cursor-high");
        let store = HistoryStore::with_dir(&dir, 100);
        store.append(record_at("a", 1, "a")).unwrap();
        let page = store
            .list_page(
                Some(&HistoryCursor {
                    seq: make_seq(999, 0),
                }),
                10,
            )
            .unwrap();
        assert!(page.items.is_empty());
        assert!(!page.has_more);
    }

    /// §10 测试 8：retention 删最旧段后，低位游标 → 空页。
    #[test]
    fn cursor_behind_oldest_returns_empty() {
        let dir = test_dir("cursor-low");
        let store = HistoryStore::with_dir(&dir, 1000);
        for i in 0..1001 {
            store
                .append(record_at(&format!("id{i}"), i as i64, "s"))
                .unwrap();
        }
        assert!(!dir.join("seg-000000.jsonl").exists());
        let stale = HistoryCursor {
            seq: make_seq(0, 100),
        };
        let page = store.list_page(Some(&stale), 10).unwrap();
        assert!(page.items.is_empty());
        assert!(!page.has_more);
        assert!(store.get(make_seq(0, 100)).unwrap().is_none());
    }

    /// §10 测试 12：命中 / 未命中 / 清空后 / 段已裁掉。
    #[test]
    fn get_by_seq() {
        let dir = test_dir("get");
        let store = HistoryStore::with_dir(&dir, 100);
        let stored = store.append(record_at("x", 1, "hi")).unwrap();
        let got = store.get(stored.seq).unwrap().unwrap();
        assert_eq!(got.source, "hi");
        assert_eq!(got.seq, stored.seq);
        assert!(store.get(stored.seq.wrapping_add(1)).unwrap().is_none());
        // seq=0 合法
        assert_eq!(stored.seq, make_seq(0, 0));
        assert_eq!(store.get(0).unwrap().unwrap().id, "x");

        store.clear().unwrap();
        assert!(store.get(stored.seq).unwrap().is_none());
        assert!(store.list_page(None, 10).unwrap().items.is_empty());
        let again = store.append(record_at("y", 2, "again")).unwrap();
        assert!(again.seq > stored.seq);
        assert_eq!(list_seg_names(&dir), vec!["seg-000001.jsonl".to_string()]);

        // 段被 retention 裁掉后 get 返回 None
        let dir2 = test_dir("get-trimmed");
        let store2 = HistoryStore::with_dir(&dir2, 1000);
        let early = store2.append(record_at("early", 0, "e")).unwrap();
        for i in 1..1001 {
            store2
                .append(record_at(&format!("id{i}"), i as i64, "s"))
                .unwrap();
        }
        assert!(!dir2.join("seg-000000.jsonl").exists());
        assert!(store2.get(early.seq).unwrap().is_none());
    }

    #[test]
    fn clear_also_removes_rewrite_tmp() {
        let dir = test_dir("clear-tmp");
        let store = HistoryStore::with_dir(&dir, 100);
        let tmp = dir.join("translations.jsonl.tmp");
        fs::write(&tmp, b"stale").unwrap();
        store.append(record_at("x", 1, "hi")).unwrap();
        store.clear().unwrap();
        assert!(list_seg_names(&dir).is_empty());
        assert!(!tmp.exists());
    }

    /// §10 测试 13：清空删光段与遗留；段号不归零；旧游标 / seq 被守卫挡住。
    #[test]
    fn clear_removes_all_segments_and_legacy() {
        let dir = test_dir("clear-all");
        let store = HistoryStore::with_dir(&dir, 501);
        for i in 0..501 {
            store
                .append(record_at(&format!("id{i}"), i as i64, "s"))
                .unwrap();
        }
        assert_eq!(
            list_seg_names(&dir),
            vec![
                "seg-000000.jsonl".to_string(),
                "seg-000001.jsonl".to_string()
            ]
        );
        let max_ord_before = 1u32;
        let page = store.list_page(None, 1).unwrap();
        let old_cursor = HistoryCursor::of(&page.items[0]);
        let old_seq = page.items[0].seq;

        let legacy = dir.join("translations.jsonl");
        let tmp = dir.join("translations.jsonl.tmp");
        fs::write(&legacy, b"legacy\n").unwrap();
        fs::write(&tmp, b"stale").unwrap();

        store.clear().unwrap();
        assert!(list_seg_names(&dir).is_empty());
        assert!(!legacy.exists());
        assert!(!tmp.exists());
        let empty = store.list_page(None, 10).unwrap();
        assert!(empty.items.is_empty());
        assert!(!empty.has_more);

        // 旧游标 / seq：清空后目录无段 → 空页 / None（守卫 + 无 newest）
        let stale_page = store.list_page(Some(&old_cursor), 10).unwrap();
        assert!(stale_page.items.is_empty());
        assert!(!stale_page.has_more);
        assert!(store.get(old_seq).unwrap().is_none());

        let again = store.append(record_at("after", 999, "n")).unwrap();
        let new_ord = ordinal_of(again.seq);
        assert!(
            new_ord > max_ord_before,
            "清空后段号应严格大于清空前最大段号，got {new_ord} vs {max_ord_before}"
        );
        assert_eq!(list_seg_names(&dir), vec!["seg-000002.jsonl".to_string()]);

        // 清空前游标落在更旧段号 → ord < oldest → 空页；旧 seq 不得命中新记录
        let guarded = store.list_page(Some(&old_cursor), 10).unwrap();
        assert!(guarded.items.is_empty());
        assert!(!guarded.has_more);
        assert!(store.get(old_seq).unwrap().is_none());
        assert_eq!(store.get(again.seq).unwrap().unwrap().id, "after");
    }

    /// `max_records = 3` → `segment_records = 3`、`max_segments = 1`；5 条后只剩最新段里 2 条。
    #[test]
    fn trim_keeps_newest_within_max() {
        let dir = test_dir("trim");
        let store = HistoryStore::with_dir(&dir, 3);
        for i in 0..5 {
            store
                .append(record_at(&format!("id{i}"), i as i64, &format!("s{i}")))
                .unwrap();
        }
        let all = store.list_page(None, 100).unwrap();
        assert_eq!(ids(&all), vec!["id4", "id3"]);

        let reloaded = HistoryStore::with_dir(store.dir(), 3);
        assert_eq!(
            ids(&reloaded.list_page(None, 100).unwrap()),
            vec!["id4", "id3"]
        );
    }

    #[test]
    fn rotate_creates_next_segment_at_capacity() {
        let dir = test_dir("rotate");
        let store = HistoryStore::with_dir(&dir, 501);
        for i in 0..500 {
            store
                .append(record_at(&format!("id{i}"), i as i64, "s"))
                .unwrap();
        }
        assert_eq!(list_seg_names(&dir), vec!["seg-000000.jsonl".to_string()]);
        store.append(record_at("next", 500, "n")).unwrap();
        assert_eq!(
            list_seg_names(&dir),
            vec![
                "seg-000000.jsonl".to_string(),
                "seg-000001.jsonl".to_string()
            ]
        );
        let seg0 = fs::read_to_string(dir.join("seg-000000.jsonl")).unwrap();
        assert!(!seg0.contains("\"id\":\"next\""));
        let seg1 = fs::read_to_string(dir.join("seg-000001.jsonl")).unwrap();
        assert!(seg1.contains("\"id\":\"next\""));
    }

    #[test]
    fn append_at_capacity_does_not_rewrite_any_file() {
        let dir = test_dir("no-rewrite");
        let store = HistoryStore::with_dir(&dir, 1500);
        for i in 0..1501 {
            store
                .append(record_at(&format!("id{i}"), i as i64, "s"))
                .unwrap();
        }
        // seg-0 已 unlink；seg-1/seg-2 封存；seg-3 活动 1 条
        assert!(!dir.join("seg-000000.jsonl").exists());
        let seg1 = dir.join("seg-000001.jsonl");
        let seg2 = dir.join("seg-000002.jsonl");
        let fp1 = file_fingerprint(&seg1);
        let fp2 = file_fingerprint(&seg2);
        #[cfg(unix)]
        let inode1 = fs::metadata(&seg1).unwrap().ino();
        #[cfg(unix)]
        let inode2 = fs::metadata(&seg2).unwrap().ino();

        for i in 0..499 {
            store
                .append(record_at(&format!("extra{i}"), 2000 + i as i64, "x"))
                .unwrap();
        }
        assert_eq!(file_fingerprint(&seg1), fp1);
        assert_eq!(file_fingerprint(&seg2), fp2);
        #[cfg(unix)]
        {
            assert_eq!(fs::metadata(&seg1).unwrap().ino(), inode1);
            assert_eq!(fs::metadata(&seg2).unwrap().ino(), inode2);
        }
    }

    #[test]
    fn retention_drops_oldest_segment_by_unlink() {
        let dir = test_dir("retention-small");
        let store = HistoryStore::with_dir(&dir, 3);
        for i in 0..5 {
            store
                .append(record_at(&format!("id{i}"), i as i64, "s"))
                .unwrap();
        }
        assert_eq!(
            ids(&store.list_page(None, 100).unwrap()),
            vec!["id4", "id3"]
        );
        assert!(!dir.join("seg-000000.jsonl").exists());
        assert!(dir.join("seg-000001.jsonl").exists());

        let dir2 = test_dir("retention-multi");
        let store2 = HistoryStore::with_dir(&dir2, 1000);
        for i in 0..1001 {
            store2
                .append(record_at(&format!("id{i}"), i as i64, "s"))
                .unwrap();
        }
        assert!(!dir2.join("seg-000000.jsonl").exists());
        assert!(dir2.join("seg-000001.jsonl").exists());
        assert!(dir2.join("seg-000002.jsonl").exists());
    }

    /// §10 测试 11：重启后（含末行半写入）`seq` 不重复，且写入的记录**在磁盘上**是真的
    /// 落了盘（不能只看 LRU 缓存：缓存会遮蔽残行粘连导致的坏行）。
    #[test]
    fn restart_assigns_unique_seq() {
        let dir = test_dir("restart-seq");
        let first_seqs: Vec<u64>;
        {
            let store = HistoryStore::with_dir(&dir, 100);
            first_seqs = (0..3)
                .map(|i| {
                    store
                        .append(record_at(&format!("id{i}"), i as i64, "s"))
                        .unwrap()
                        .seq
                })
                .collect();
        }
        // 崩溃残留：末行半写入（无结尾换行）
        {
            let path = dir.join("seg-000000.jsonl");
            let mut file = OpenOptions::new().append(true).open(&path).unwrap();
            file.write_all(b"{\"seq\":").unwrap();
            file.flush().unwrap();
        }
        let store = HistoryStore::with_dir(&dir, 100);
        let again = store.append(record_at("new", 99, "n")).unwrap();
        assert!(!first_seqs.contains(&again.seq));
        assert_eq!(again.seq, make_seq(0, 3));
        let page = store.list_page(None, 100).unwrap();
        assert_eq!(ids(&page), vec!["new", "id2", "id1", "id0"]);
        drop(store);

        // 关键回归：重新打开（丢掉内存缓存）后，磁盘上的记录仍可解析
        let reopened = HistoryStore::with_dir(&dir, 100);
        let page = reopened.list_page(None, 100).unwrap();
        assert_eq!(ids(&page), vec!["new", "id2", "id1", "id0"]);
        let body = fs::read_to_string(dir.join("seg-000000.jsonl")).unwrap();
        assert!(body.ends_with('\n'), "段文件应以换行结尾: {body:?}");
        assert!(
            !body.contains("{\"seq\":{\"seq\""),
            "残行不应与新记录粘连: {body:?}"
        );
        assert_eq!(page.items[0].seq, make_seq(0, 3));
    }

    /// 半行修复：只截掉残缺的最后一行，前面完整行全部保留。
    #[test]
    fn torn_tail_is_truncated_on_open() {
        let dir = test_dir("torn-tail");
        let store = HistoryStore::with_dir(&dir, 100);
        for i in 0..3 {
            store
                .append(record_at(&format!("id{i}"), i as i64, "s"))
                .unwrap();
        }
        drop(store);

        let path = dir.join("seg-000000.jsonl");
        let good_len = fs::metadata(&path).unwrap().len();
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(b"{\"seq\":1,\"id\":\"tor").unwrap();
        file.flush().unwrap();
        drop(file);

        let store = HistoryStore::with_dir(&dir, 100);
        assert_eq!(fs::metadata(&path).unwrap().len(), good_len, "残行应被截掉");
        let page = store.list_page(None, 10).unwrap();
        assert_eq!(ids(&page), vec!["id2", "id1", "id0"]);
        // 新记录紧跟在完整行之后，独占一行
        let next = store.append(record_at("after", 9, "a")).unwrap();
        assert_eq!(next.seq, make_seq(0, 3));
        drop(store);
        let reopened = HistoryStore::with_dir(&dir, 100);
        assert_eq!(
            ids(&reopened.list_page(None, 10).unwrap()),
            vec!["after", "id2", "id1", "id0"]
        );
    }

    /// 打开失败不再永久禁用：故障消失后第一次访问自动重试成功。
    #[test]
    fn open_failure_is_retried_on_next_access() {
        let dir = test_dir("retry-open");
        let blocker = dir.join("history");
        fs::write(&blocker, b"x").unwrap();
        let store = HistoryStore::with_dir(&blocker, 10);
        assert!(store.append(record_at("a", 1, "a")).is_err());
        assert!(store.list_page(None, 10).is_err());

        // 故障消失（普通文件 → 目录）后无需重建 store
        fs::remove_file(&blocker).unwrap();
        store.append(record_at("b", 2, "b")).unwrap();
        assert_eq!(ids(&store.list_page(None, 10).unwrap()), vec!["b"]);
    }

    #[test]
    fn legacy_files_are_ignored_and_removed() {
        let dir = test_dir("legacy");
        fs::write(
            dir.join("translations.jsonl"),
            b"{\"seq\":1,\"id\":\"old\",\"created_at\":1,\"source_id\":\"m\",\"label\":\"M\",\"source\":\"x\",\"translated\":\"y\"}\nnot-json\n",
        )
        .unwrap();
        fs::write(dir.join("translations.jsonl.tmp"), b"stale").unwrap();
        let store = HistoryStore::with_dir(&dir, 100);
        assert!(!dir.join("translations.jsonl").exists());
        assert!(!dir.join("translations.jsonl.tmp").exists());
        assert!(list_seg_names(&dir).is_empty());
        assert!(store.list_page(None, 10).unwrap().items.is_empty());
    }

    #[test]
    fn open_trims_overflow_segments() {
        let dir = test_dir("open-trim");
        {
            let store = HistoryStore::with_dir(&dir, 1500);
            for i in 0..1500 {
                store
                    .append(record_at(&format!("id{i}"), i as i64, "s"))
                    .unwrap();
            }
            assert_eq!(list_seg_names(&dir).len(), 3);
        }
        let reopened = HistoryStore::with_dir(&dir, 500);
        assert_eq!(list_seg_names(&dir).len(), 1);
        let page = reopened.list_page(None, 10_000).unwrap();
        assert!(page.items.len() <= 500);
        assert!(!page.items.is_empty());
    }

    #[test]
    fn default_dir_lives_under_data_local_history() {
        let dir = default_history_dir().expect("dirs");
        assert!(dir.ends_with("history"));
    }

    #[test]
    fn missing_file_lists_empty_and_get_is_none() {
        let dir = test_dir("missing");
        let store = HistoryStore::with_dir(&dir, 50);
        assert!(store.list_page(None, 10).unwrap().items.is_empty());
        assert!(store.get(0).unwrap().is_none());
        store.clear().unwrap();
    }

    #[test]
    fn skips_blank_and_malformed_jsonl_lines() {
        let dir = test_dir("corrupt");
        let path = dir.join("seg-000000.jsonl");
        let good = TranslationRecord {
            seq: make_seq(0, 7),
            id: "ok".into(),
            created_at: 50,
            source_id: "mic".into(),
            label: "Mic".into(),
            source: "hello".into(),
            translated: "译:hello".into(),
        };
        let good_json = serde_json::to_string(&good).unwrap();
        fs::write(
            &path,
            format!("\nnot-json\n{good_json}\n{{}}\n   \n{{\"id\":1}}\n"),
        )
        .unwrap();
        let store = HistoryStore::with_dir(&dir, 50);
        let page = store.list_page(None, 10).unwrap();
        assert_eq!(ids(&page), vec!["ok"]);
        assert_eq!(store.get(make_seq(0, 7)).unwrap().unwrap().source, "hello");
    }

    /// §10 测试 9：段内坏行不影响按 seq 二分的跨页结果。
    #[test]
    fn malformed_lines_do_not_break_cursor() {
        let dir = test_dir("bad-cursor");
        let mut body = String::new();
        for i in 0..6 {
            if i % 2 == 1 {
                body.push_str("not-json\n{}\n");
            }
            let rec = TranslationRecord {
                seq: make_seq(0, i),
                id: format!("id{i}"),
                created_at: i as i64,
                source_id: "mic".into(),
                label: "Mic".into(),
                source: format!("s{i}"),
                translated: format!("译:s{i}"),
            };
            body.push_str(&serde_json::to_string(&rec).unwrap());
            body.push('\n');
        }
        fs::write(dir.join("seg-000000.jsonl"), body).unwrap();
        let store = HistoryStore::with_dir(&dir, 100);
        let first = store.list_page(None, 2).unwrap();
        assert_eq!(ids(&first), vec!["id5", "id4"]);
        let second = store
            .list_page(Some(&HistoryCursor::of(&first.items[1])), 2)
            .unwrap();
        assert_eq!(ids(&second), vec!["id3", "id2"]);
        let third = store
            .list_page(Some(&HistoryCursor::of(&second.items[1])), 2)
            .unwrap();
        assert_eq!(ids(&third), vec!["id1", "id0"]);
        assert!(!third.has_more);
        let all: Vec<&str> = ids(&first)
            .into_iter()
            .chain(ids(&second))
            .chain(ids(&third))
            .collect();
        assert_eq!(all, vec!["id5", "id4", "id3", "id2", "id1", "id0"]);
    }

    /// §10 测试 10：空/缺失段跳过；更旧段全空时 has_more=false。
    #[test]
    fn empty_or_missing_segment_is_skipped() {
        let dir = test_dir("empty-missing");
        let mut seg0 = String::new();
        for i in 0..2 {
            let rec = TranslationRecord {
                seq: make_seq(0, i),
                id: format!("a{i}"),
                created_at: i as i64,
                source_id: "mic".into(),
                label: "Mic".into(),
                source: format!("a{i}"),
                translated: format!("译:a{i}"),
            };
            seg0.push_str(&serde_json::to_string(&rec).unwrap());
            seg0.push('\n');
        }
        fs::write(dir.join("seg-000000.jsonl"), seg0).unwrap();
        // 中间段缺失（无 seg-1）
        let mut seg2 = String::new();
        for i in 0..2 {
            let rec = TranslationRecord {
                seq: make_seq(2, i),
                id: format!("c{i}"),
                created_at: 20 + i as i64,
                source_id: "mic".into(),
                label: "Mic".into(),
                source: format!("c{i}"),
                translated: format!("译:c{i}"),
            };
            seg2.push_str(&serde_json::to_string(&rec).unwrap());
            seg2.push('\n');
        }
        fs::write(dir.join("seg-000002.jsonl"), seg2).unwrap();
        let store = HistoryStore::with_dir(&dir, 1500);
        let page = store.list_page(None, 100).unwrap();
        assert_eq!(ids(&page), vec!["c1", "c0", "a1", "a0"]);
        assert!(!page.has_more);

        // 空活动段 + 非空更旧段：读满后 peek 到空更旧段 → has_more=false
        let dir2 = test_dir("empty-older");
        fs::write(dir2.join("seg-000000.jsonl"), b"").unwrap();
        let mut seg1 = String::new();
        for i in 0..2 {
            let rec = TranslationRecord {
                seq: make_seq(1, i),
                id: format!("b{i}"),
                created_at: i as i64,
                source_id: "mic".into(),
                label: "Mic".into(),
                source: format!("b{i}"),
                translated: format!("译:b{i}"),
            };
            seg1.push_str(&serde_json::to_string(&rec).unwrap());
            seg1.push('\n');
        }
        fs::write(dir2.join("seg-000001.jsonl"), seg1).unwrap();
        let store2 = HistoryStore::with_dir(&dir2, 501);
        let full = store2.list_page(None, 2).unwrap();
        assert_eq!(ids(&full), vec!["b1", "b0"]);
        assert!(!full.has_more);
    }

    /// §10 测试 16：读满一页且下一个更旧段非空 → has_more=true。
    #[test]
    fn has_more_true_when_next_older_segment_nonempty() {
        let dir = test_dir("has-more-peek");
        let store = HistoryStore::with_dir(&dir, 501);
        for i in 0..501 {
            store
                .append(record_at(&format!("id{i}"), i as i64, "s"))
                .unwrap();
        }
        let page = store.list_page(None, 1).unwrap();
        assert_eq!(ids(&page), vec!["id500"]);
        assert!(page.has_more);
        let next = store
            .list_page(Some(&HistoryCursor::of(&page.items[0])), 1)
            .unwrap();
        assert_eq!(ids(&next), vec!["id499"]);
        assert_eq!(next.items[0].seq, make_seq(0, 499));

        // 自然可达变体：1500 / 1001 条，limit=501 正好读完 seg2+seg1
        let dir2 = test_dir("has-more-peek-1500");
        let store2 = HistoryStore::with_dir(&dir2, 1500);
        for i in 0..1001 {
            store2
                .append(record_at(&format!("id{i}"), i as i64, "s"))
                .unwrap();
        }
        let page2 = store2.list_page(None, 501).unwrap();
        assert_eq!(page2.items.len(), 501);
        assert!(page2.has_more);
        assert_eq!(page2.items[0].id, "id1000");
        assert_eq!(page2.items[500].id, "id500");
        let next2 = store2
            .list_page(Some(&HistoryCursor::of(&page2.items[500])), 1)
            .unwrap();
        assert_eq!(ids(&next2), vec!["id499"]);
    }

    #[test]
    fn seq_index_bits_is_shared_constant() {
        assert_eq!(SEQ_INDEX_BITS, 20);
        let seq = ((1u64) << SEQ_INDEX_BITS) | 3;
        assert_eq!(seq >> SEQ_INDEX_BITS, 1);
        assert_eq!(seq & ((1u64 << SEQ_INDEX_BITS) - 1), 3);
    }
}

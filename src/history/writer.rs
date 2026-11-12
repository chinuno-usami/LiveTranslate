//! 有界异步旁路写入。
//!
//! 实时字幕入队路径只做非阻塞 `try_send`；写盘在专用线程执行。
//! 队列满则跳过并累计计数；`HistoryStore::append` 失败只 `warn`，
//! 不回传到调用方，不影响 `SubtitleState` / 浮窗 / OSC / SteamVR。
//!
//! 隐私清空走同一条写线程队列：先抬升 epoch 丢弃在途 Append，再删 jsonl/tmp，
//! 并通过 ack 通道同步等待完成，避免「清空后历史复活」。

use super::{HistoryStore, TranslationRecord};
use crate::subtitle::SubtitleLine;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TryRecvError, TrySendError};
use std::sync::Arc;
use std::thread;

/// 旁路队列默认容量（条）。
pub const DEFAULT_QUEUE_CAPACITY: usize = 128;

enum HistoryJob {
    Append {
        record: TranslationRecord,
        epoch: u64,
    },
    Clear {
        ack: SyncSender<Result<(), String>>,
    },
}

/// 将字幕行旁路投递到持久化线程的句柄（可 `Clone`）。
#[derive(Clone, Debug)]
pub struct HistoryWriter {
    tx: SyncSender<HistoryJob>,
    skipped: Arc<AtomicU64>,
    /// 清空代数：`clear_blocking` 先自增，worker 丢弃旧 epoch 的 Append。
    epoch: Arc<AtomicU64>,
}

impl HistoryWriter {
    /// 使用给定存储与队列容量启动写盘线程。
    pub fn spawn(store: HistoryStore, capacity: usize) -> Self {
        let capacity = capacity.max(1);
        let (tx, rx) = sync_channel(capacity);
        let skipped = Arc::new(AtomicU64::new(0));
        let epoch = Arc::new(AtomicU64::new(0));
        spawn_worker(store, rx, Arc::clone(&epoch));
        Self { tx, skipped, epoch }
    }

    /// 非阻塞投递；失败只记日志与计数，永不返回错误给调用方。
    pub fn offer(&self, line: &SubtitleLine) {
        self.offer_record(TranslationRecord::from_subtitle_line(line));
    }

    /// 与 [`offer`] 相同，直接接受已构造的记录（测试与命令旁路可复用）。
    pub fn offer_record(&self, record: TranslationRecord) {
        let epoch = self.epoch.load(Ordering::SeqCst);
        match self.tx.try_send(HistoryJob::Append { record, epoch }) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                let n = self.skipped.fetch_add(1, Ordering::Relaxed) + 1;
                tracing::warn!(skipped = n, "翻译历史写队列已满，跳过本条");
            }
            Err(TrySendError::Disconnected(_)) => {
                let n = self.skipped.fetch_add(1, Ordering::Relaxed) + 1;
                tracing::warn!(skipped = n, "翻译历史写线程已退出，跳过本条");
            }
        }
    }

    /// 阻塞清空：抬升 epoch、串行丢弃在途 Append、删除文件后返回。
    pub fn clear_blocking(&self) -> Result<(), String> {
        let _ = self.epoch.fetch_add(1, Ordering::SeqCst);
        let (ack_tx, ack_rx) = sync_channel(1);
        self.tx
            .send(HistoryJob::Clear { ack: ack_tx })
            .map_err(|_| "翻译历史写线程已退出".to_string())?;
        ack_rx
            .recv()
            .map_err(|_| "等待翻译历史清空确认失败".to_string())?
    }

    /// 因队列满 / 断开而跳过的累计次数。
    #[allow(dead_code)] // 供诊断与后续单测 / 命令观测
    pub fn skipped_count(&self) -> u64 {
        self.skipped.load(Ordering::Relaxed)
    }
}

fn spawn_worker(store: HistoryStore, rx: Receiver<HistoryJob>, epoch: Arc<AtomicU64>) {
    let builder = thread::Builder::new().name("history-writer".into());
    if let Err(err) = builder.spawn(move || worker_loop(store, rx, epoch)) {
        tracing::warn!(error = %err, "无法启动翻译历史写线程，持久化已禁用");
    }
}

fn worker_loop(store: HistoryStore, rx: Receiver<HistoryJob>, epoch: Arc<AtomicU64>) {
    while let Ok(job) = rx.recv() {
        match job {
            HistoryJob::Append { record, epoch: job_epoch } => {
                if job_epoch != epoch.load(Ordering::SeqCst) {
                    continue;
                }
                if let Err(err) = store.append(&record) {
                    tracing::warn!(
                        error = %err,
                        id = %record.id,
                        "翻译历史写入失败"
                    );
                }
            }
            HistoryJob::Clear { ack } => {
                let mut nested_acks = vec![ack];
                // 清空前排空队列中的在途任务，避免隐私清除后残留写入。
                loop {
                    match rx.try_recv() {
                        Ok(HistoryJob::Append { .. }) => {}
                        Ok(HistoryJob::Clear { ack: nested }) => nested_acks.push(nested),
                        Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
                    }
                }
                let result = store.clear().map_err(|e| e.to_string());
                for nested in nested_acks {
                    let _ = nested.send(result.clone());
                }
            }
        }
    }
}

/// 可选旁路：未启用时为空操作。
#[derive(Clone, Debug, Default)]
pub struct OptionalHistoryWriter {
    inner: Option<HistoryWriter>,
}

impl OptionalHistoryWriter {
    pub fn new(inner: Option<HistoryWriter>) -> Self {
        Self { inner }
    }

    pub fn try_spawn_default() -> Self {
        Self::try_spawn_default_with_store().1
    }

    /// 打开默认存储并启动写线程；同时返回可读的 [`HistoryStore`]（与写线程同路径）。
    /// 打开失败时两者均为空，仅 warn。
    pub fn try_spawn_default_with_store() -> (Option<HistoryStore>, Self) {
        match HistoryStore::open_default() {
            Ok(store) => {
                let writer = HistoryWriter::spawn(store.clone(), DEFAULT_QUEUE_CAPACITY);
                (Some(store), Self::new(Some(writer)))
            }
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    "无法打开翻译历史存储，持久化已禁用"
                );
                (None, Self::default())
            }
        }
    }

    pub fn offer(&self, line: &SubtitleLine) {
        if let Some(writer) = &self.inner {
            writer.offer(line);
        }
    }

    /// 阻塞清空本地历史；无写线程时返回「不可用」。
    pub fn clear_blocking(&self) -> Result<(), String> {
        match &self.inner {
            Some(writer) => writer.clear_blocking(),
            None => Err("翻译历史存储不可用".to_string()),
        }
    }

    #[allow(dead_code)] // 供诊断与后续单测 / 命令观测
    pub fn skipped_count(&self) -> u64 {
        self.inner.as_ref().map(|w| w.skipped_count()).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::store::HistoryStore;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
    use std::sync::mpsc::sync_channel;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    fn test_dir(tag: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, AtomicOrdering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "livetranslate-history-writer-{}-{}-{}-{}",
            std::process::id(),
            nanos,
            n,
            tag
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create test dir");
        dir
    }

    fn wait_until(mut pred: impl FnMut() -> bool, timeout: Duration) -> bool {
        let start = std::time::Instant::now();
        while start.elapsed() < timeout {
            if pred() {
                return true;
            }
            thread::sleep(Duration::from_millis(5));
        }
        pred()
    }

    #[test]
    fn offer_persists_without_touching_caller() {
        let dir = test_dir("persist");
        let path = dir.join("translations.jsonl");
        let store = HistoryStore::with_path(&path, 100);
        let writer = HistoryWriter::spawn(store, 8);
        let line = SubtitleLine::new("mic", "Mic", "hello".into(), "你好".into());
        writer.offer(&line);

        assert!(wait_until(
            || {
                path.exists()
                    && fs::read_to_string(&path)
                        .map(|s| s.contains("hello"))
                        .unwrap_or(false)
            },
            Duration::from_secs(2)
        ));
        // 调用方持有的 line 未被改写
        assert_eq!(line.source, "hello");
        assert_eq!(line.translated, "你好");
    }

    #[test]
    fn full_queue_skips_and_counts() {
        let (tx, rx) = sync_channel::<HistoryJob>(1);
        let skipped = Arc::new(AtomicU64::new(0));
        let epoch = Arc::new(AtomicU64::new(0));
        let writer = HistoryWriter {
            tx,
            skipped: skipped.clone(),
            epoch,
        };
        // 填满容量为 1 的队列（无消费者）
        writer.offer_record(TranslationRecord::new("a", "A", "one", "一"));
        writer.offer_record(TranslationRecord::new("b", "B", "two", "二"));
        assert_eq!(writer.skipped_count(), 1);
        // 释放接收端，避免 Drop 噪音
        drop(rx);
    }

    #[test]
    fn append_io_failure_is_swallowed_by_worker() {
        // 指向「父路径是普通文件」使 create_dir_all / open 失败
        let dir = test_dir("iofail");
        let blocker = dir.join("not-a-dir");
        fs::write(&blocker, b"x").unwrap();
        let bad_path = blocker.join("translations.jsonl");
        let store = HistoryStore::with_path(bad_path, 10);
        let writer = HistoryWriter::spawn(store, 4);
        writer.offer(&SubtitleLine::new("mic", "Mic", "x".into(), "y".into()));
        // 给 worker 时间尝试写盘；不应 panic，skipped 仍为 0（已入队）
        thread::sleep(Duration::from_millis(100));
        assert_eq!(writer.skipped_count(), 0);
    }

    #[test]
    fn clear_blocking_discards_queued_appends_and_deletes_file() {
        let dir = test_dir("clear-race");
        let path = dir.join("translations.jsonl");
        let store = HistoryStore::with_path(&path, 100);
        // 容量 1：先塞满一条 Append，再 clear；worker 被 sleep 占住前先排队
        let writer = HistoryWriter::spawn(store.clone(), 1);

        // 先写入一条可观测内容
        writer.offer(&SubtitleLine::new("mic", "Mic", "keep-me".into(), "留".into()));
        assert!(wait_until(
            || {
                path.exists()
                    && fs::read_to_string(&path)
                        .map(|s| s.contains("keep-me"))
                        .unwrap_or(false)
            },
            Duration::from_secs(2)
        ));

        // 无消费者式填满后再 clear：即使队列里还有 Append，清空后文件也不该复活
        // 用阻塞 clear；同时并发 offer 若干条
        for i in 0..32 {
            writer.offer(&SubtitleLine::new(
                "mic",
                "Mic",
                format!("ghost-{i}"),
                format!("鬼{i}"),
            ));
        }
        writer.clear_blocking().expect("clear");

        assert!(!path.exists(), "clear 后 jsonl 应删除");
        // 给可能残留的旧 epoch 写入一点时间；文件不应被重建出 ghost 内容
        thread::sleep(Duration::from_millis(80));
        if path.exists() {
            let body = fs::read_to_string(&path).unwrap_or_default();
            assert!(
                !body.contains("ghost-"),
                "清空后不应再落盘清空前的在途记录: {body}"
            );
        }

        // 清空之后的新记录仍可写入
        writer.offer(&SubtitleLine::new("mic", "Mic", "after".into(), "后".into()));
        assert!(wait_until(
            || {
                path.exists()
                    && fs::read_to_string(&path)
                        .map(|s| s.contains("after"))
                        .unwrap_or(false)
            },
            Duration::from_secs(2)
        ));
    }

    #[test]
    fn optional_clear_without_writer_is_unavailable() {
        let writer = OptionalHistoryWriter::default();
        let err = writer.clear_blocking().unwrap_err();
        assert!(err.contains("不可用"), "{err}");
    }
}

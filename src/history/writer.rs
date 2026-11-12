//! 有界异步旁路写入。
//!
//! 实时字幕入队路径只做非阻塞 `try_send`；写盘在专用线程执行。
//! 队列满则跳过并累计计数；`HistoryStore::append` 失败只 `warn`，
//! 不回传到调用方，不影响 `SubtitleState` / 浮窗 / OSC / SteamVR。
//!
//! 入队只搬运字幕行本身，UUID 与记录构造推迟到写线程：队列满时不会白白生成
//! 一个记录再丢掉。
//!
//! 隐私清空走同一条写线程队列：先抬升 epoch 丢弃在途 Append，再删 jsonl/tmp，
//! 并通过 ack 通道同步等待完成，避免「清空后历史复活」。退出前用
//! [`HistoryWriter::flush_blocking`] 把队列刷盘，避免丢掉最后几条记录。

use super::{HistoryStore, TranslationRecord};
use crate::subtitle::SubtitleLine;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TryRecvError, TrySendError};
use std::sync::Arc;
use std::thread;

/// 旁路队列默认容量（条）。
pub const DEFAULT_QUEUE_CAPACITY: usize = 128;

enum HistoryJob {
    /// 实时路径只搬运字幕行，记录（含 UUID）在写线程里构造
    Append {
        line: SubtitleLine,
        epoch: u64,
    },
    Clear {
        ack: SyncSender<Result<(), String>>,
    },
    Flush {
        ack: SyncSender<()>,
    },
}

/// 将字幕行旁路投递到持久化线程的句柄（可 `Clone`）。
#[derive(Clone, Debug)]
pub struct HistoryWriter {
    tx: SyncSender<HistoryJob>,
    skipped: Arc<AtomicU64>,
    /// 清空代数：`clear_blocking` 先自增，worker 丢弃旧 epoch 的 Append。
    epoch: Arc<AtomicU64>,
    /// 写线程已退出：只提示一次，避免每条字幕都 warn 刷屏。
    disabled: Arc<AtomicBool>,
}

impl HistoryWriter {
    /// 使用给定存储与队列容量启动写盘线程。
    pub fn spawn(store: HistoryStore, capacity: usize) -> Self {
        let capacity = capacity.max(1);
        let (tx, rx) = sync_channel(capacity);
        let skipped = Arc::new(AtomicU64::new(0));
        let epoch = Arc::new(AtomicU64::new(0));
        spawn_worker(store, rx, Arc::clone(&epoch));
        Self {
            tx,
            skipped,
            epoch,
            disabled: Arc::new(AtomicBool::new(false)),
        }
    }

    /// 非阻塞投递；失败只记日志与计数，永不返回错误给调用方。
    pub fn offer(&self, line: &SubtitleLine) {
        let epoch = self.epoch.load(Ordering::SeqCst);
        match self.tx.try_send(HistoryJob::Append {
            line: line.clone(),
            epoch,
        }) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                let n = self.skipped.fetch_add(1, Ordering::Relaxed) + 1;
                tracing::warn!(skipped = n, "翻译历史写队列已满，跳过本条");
            }
            Err(TrySendError::Disconnected(_)) => {
                let n = self.skipped.fetch_add(1, Ordering::Relaxed) + 1;
                // 线程已退出后逐条 warn 只会刷屏，降级提示一次即可
                if !self.disabled.swap(true, Ordering::Relaxed) {
                    tracing::warn!(skipped = n, "翻译历史写线程已退出，翻译历史持久化已停用");
                }
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

    /// 等待队列中已受理的记录全部落盘（退出前调用）。
    ///
    /// 队列是 FIFO，前面所有 Append 处理完才会处理这条 Flush。
    pub fn flush_blocking(&self) -> Result<(), String> {
        let (ack_tx, ack_rx) = sync_channel(1);
        self.tx
            .send(HistoryJob::Flush { ack: ack_tx })
            .map_err(|_| "翻译历史写线程已退出".to_string())?;
        ack_rx
            .recv()
            .map_err(|_| "等待翻译历史落盘确认失败".to_string())
    }

    /// 因队列满 / 断开而跳过的累计次数。
    #[allow(dead_code)] // 供诊断与后续单测 / 命令观测
    pub fn skipped_count(&self) -> u64 {
        self.skipped.load(Ordering::Relaxed)
    }

    /// 仅测试：构造一个接收端已释放的句柄（模拟写线程启动失败 / 已退出）。
    #[cfg(test)]
    pub fn disconnected() -> Self {
        let (tx, rx) = sync_channel(1);
        drop(rx);
        Self {
            tx,
            skipped: Arc::new(AtomicU64::new(0)),
            epoch: Arc::new(AtomicU64::new(0)),
            disabled: Arc::new(AtomicBool::new(false)),
        }
    }
}

fn spawn_worker(store: HistoryStore, rx: Receiver<HistoryJob>, epoch: Arc<AtomicU64>) {
    let builder = thread::Builder::new().name("history-writer".into());
    if let Err(err) = builder.spawn(move || worker_loop(store, rx, epoch)) {
        // 线程没起来时接收端随闭包一起释放，后续 offer 走 Disconnected、
        // clear/flush 走 Err，调用方据此降级（清空回退直接删文件）。
        tracing::warn!(error = %err, "无法启动翻译历史写线程，持久化已禁用");
    }
}

fn worker_loop(store: HistoryStore, rx: Receiver<HistoryJob>, epoch: Arc<AtomicU64>) {
    while let Ok(job) = rx.recv() {
        match job {
            HistoryJob::Append {
                line,
                epoch: job_epoch,
            } => {
                if job_epoch != epoch.load(Ordering::SeqCst) {
                    continue;
                }
                let record = TranslationRecord::from_subtitle_line(&line);
                if let Err(err) = store.append(&record) {
                    tracing::warn!(
                        error = %err,
                        id = %record.id,
                        "翻译历史写入失败"
                    );
                }
            }
            HistoryJob::Flush { ack } => {
                // FIFO：走到这里说明之前的 Append 都已落盘
                let _ = ack.send(());
            }
            HistoryJob::Clear { ack } => {
                let mut clear_acks = vec![ack];
                let mut flush_acks = Vec::new();
                // 清空前排空队列中的在途任务，避免隐私清除后残留写入。
                loop {
                    match rx.try_recv() {
                        Ok(HistoryJob::Append { .. }) => {}
                        Ok(HistoryJob::Clear { ack: nested }) => clear_acks.push(nested),
                        Ok(HistoryJob::Flush { ack }) => flush_acks.push(ack),
                        Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
                    }
                }
                let result = store.clear().map_err(|e| e.to_string());
                for nested in clear_acks {
                    let _ = nested.send(result.clone());
                }
                // 被丢弃的 Append 也算「处理完」，等待落盘的调用方可以继续
                for ack in flush_acks {
                    let _ = ack.send(());
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

    /// 阻塞等待已入队记录落盘；未启用持久化时视为成功。
    pub fn flush_blocking(&self) -> Result<(), String> {
        match &self.inner {
            Some(writer) => writer.flush_blocking(),
            None => Ok(()),
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

    /// 退出前的 flush 必须保证已入队记录全部落盘（不依赖 sleep 猜测）
    #[test]
    fn flush_blocking_persists_queued_records() {
        let dir = test_dir("flush");
        let path = dir.join("translations.jsonl");
        let store = HistoryStore::with_path(&path, 100);
        let writer = HistoryWriter::spawn(store.clone(), 16);
        for i in 0..8 {
            writer.offer(&SubtitleLine::new(
                "mic",
                "Mic",
                format!("s{i}"),
                format!("t{i}"),
            ));
        }
        writer.flush_blocking().expect("flush");

        let page = store.list_page(None, 100).unwrap();
        assert_eq!(page.items.len(), 8);
        assert_eq!(writer.skipped_count(), 0);
        // 未启用持久化时 flush 视为成功
        assert!(OptionalHistoryWriter::default().flush_blocking().is_ok());
    }

    #[test]
    fn full_queue_skips_and_counts() {
        let (tx, rx) = sync_channel::<HistoryJob>(1);
        let writer = HistoryWriter {
            tx,
            skipped: Arc::new(AtomicU64::new(0)),
            epoch: Arc::new(AtomicU64::new(0)),
            disabled: Arc::new(AtomicBool::new(false)),
        };
        // 填满容量为 1 的队列（无消费者）
        writer.offer(&SubtitleLine::new("a", "A", "one".into(), "一".into()));
        writer.offer(&SubtitleLine::new("b", "B", "two".into(), "二".into()));
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
        // 写盘失败也要把任务处理掉，让 flush 能返回；skipped 仍为 0（已入队）
        writer.flush_blocking().expect("flush");
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
        writer.offer(&SubtitleLine::new(
            "mic",
            "Mic",
            "keep-me".into(),
            "留".into(),
        ));
        writer.flush_blocking().expect("flush");
        assert!(fs::read_to_string(&path)
            .map(|s| s.contains("keep-me"))
            .unwrap_or(false));

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
        writer.offer(&SubtitleLine::new(
            "mic",
            "Mic",
            "after".into(),
            "后".into(),
        ));
        writer.flush_blocking().expect("flush");
        assert!(fs::read_to_string(&path)
            .map(|s| s.contains("after"))
            .unwrap_or(false));
    }

    #[test]
    fn optional_clear_without_writer_is_unavailable() {
        let writer = OptionalHistoryWriter::default();
        let err = writer.clear_blocking().unwrap_err();
        assert!(err.contains("不可用"), "{err}");
    }
}

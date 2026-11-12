//! 翻译历史记录（持久化层模型与本地 JSONL 存储）。
//!
//! 与滚动浮窗用的 [`crate::subtitle::SubtitleLine`] 解耦：
//! 实时显示仍按 `max_lines` 裁剪；本类型带稳定 id 与时间戳，供本地存储与 UI 查询。

pub mod store;
pub mod writer;

pub use store::{HistoryCursor, HistoryPage, HistoryStore, DEFAULT_MAX_RECORDS};
pub use writer::OptionalHistoryWriter;

use crate::subtitle::SubtitleLine;
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

/// 一条可持久化的翻译历史记录。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranslationRecord {
    /// 稳定唯一 id（UUID v4）
    pub id: String,
    /// 创建时间：Unix 纪元毫秒（UTC）
    pub created_at: i64,
    pub source_id: String,
    pub label: String,
    pub source: String,
    pub translated: String,
}

impl TranslationRecord {
    /// 新建记录：自动生成 id 与当前 `created_at`。
    pub fn new(
        source_id: impl Into<String>,
        label: impl Into<String>,
        source: impl Into<String>,
        translated: impl Into<String>,
    ) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            created_at: unix_now_millis(),
            source_id: source_id.into(),
            label: label.into(),
            source: source.into(),
            translated: translated.into(),
        }
    }

    /// 从滚动字幕行派生历史记录（不共享生命周期/身份，仅复制文本字段）。
    pub fn from_subtitle_line(line: &SubtitleLine) -> Self {
        Self::new(
            line.source_id.as_str(),
            line.label.as_str(),
            line.source.as_str(),
            line.translated.as_str(),
        )
    }
}

fn unix_now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_fills_id_and_timestamp() {
        let before = unix_now_millis();
        let record = TranslationRecord::new("mic", "麦克风", "hello", "你好");
        let after = unix_now_millis();

        assert!(!record.id.is_empty());
        assert!(uuid::Uuid::parse_str(&record.id).is_ok());
        assert!(record.created_at >= before && record.created_at <= after);
        assert_eq!(record.source_id, "mic");
        assert_eq!(record.label, "麦克风");
        assert_eq!(record.source, "hello");
        assert_eq!(record.translated, "你好");
    }

    #[test]
    fn from_subtitle_line_copies_fields_without_coupling_identity() {
        let line = SubtitleLine::new("sys", "系统音频", "world".into(), "世界".into());
        let record = TranslationRecord::from_subtitle_line(&line);

        assert_eq!(record.source_id, line.source_id);
        assert_eq!(record.label, line.label);
        assert_eq!(record.source, line.source);
        assert_eq!(record.translated, line.translated);
        // SubtitleLine 本身没有 id/时间戳；记录侧独立持有
        assert!(uuid::Uuid::parse_str(&record.id).is_ok());
        assert!(record.created_at > 0);
    }

    #[test]
    fn serde_roundtrip_preserves_fields() {
        let record = TranslationRecord {
            id: "11111111-2222-3333-4444-555555555555".into(),
            created_at: 1_700_000_000_000,
            source_id: "mic".into(),
            label: "Mic".into(),
            source: "hi".into(),
            translated: "嗨".into(),
        };
        let json = serde_json::to_string(&record).expect("serialize");
        let back: TranslationRecord = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, record);
    }
}

use std::collections::VecDeque;

#[derive(Debug, Clone)]
pub struct Subtitle {
    pub source: String,
    pub translated: String,
}

impl Subtitle {
    pub fn new(source: String, translated: String) -> Self {
        Self { source, translated }
    }
}

pub struct SubtitleState {
    history: VecDeque<Subtitle>,
    max_lines: usize,
    show_source: bool,
}

impl SubtitleState {
    pub fn new(max_lines: usize, show_source: bool) -> Self {
        Self {
            history: VecDeque::with_capacity(max_lines),
            max_lines,
            show_source,
        }
    }

    /// 添加新字幕
    pub fn push(&mut self, subtitle: Subtitle) {
        self.history.push_back(subtitle);

        // 保持最多 max_lines 条
        while self.history.len() > self.max_lines {
            self.history.pop_front();
        }

        tracing::debug!(
            "Subtitle pushed. Current count: {}",
            self.history.len()
        );
    }

    /// 获取所有当前字幕
    pub fn get_all(&self) -> Vec<String> {
        self.history
            .iter()
            .map(|sub| {
                if self.show_source {
                    format!("{}\n{}", sub.source, sub.translated)
                } else {
                    sub.translated.clone()
                }
            })
            .collect()
    }

    /// 获取合并后的字幕文本
    pub fn get_text(&self) -> String {
        self.get_all().join("\n")
    }

    /// 清空历史
    pub fn clear(&mut self) {
        self.history.clear();
    }

    /// 获取当前字幕数
    pub fn len(&self) -> usize {
        self.history.len()
    }

    /// 是否为空
    pub fn is_empty(&self) -> bool {
        self.history.is_empty()
    }
}

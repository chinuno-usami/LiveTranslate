use crate::config::LabelMode;
use std::collections::VecDeque;

pub mod processor;

pub use processor::SubtitleProcessor;

/// 一条字幕（带来源信息）
#[derive(Debug, Clone, PartialEq)]
pub struct SubtitleLine {
    pub source_id: String,
    pub label: String,
    pub source: String,
    pub translated: String,
}

impl SubtitleLine {
    pub fn new(
        source_id: impl Into<String>,
        label: impl Into<String>,
        source: String,
        translated: String,
    ) -> Self {
        Self {
            source_id: source_id.into(),
            label: label.into(),
            source,
            translated,
        }
    }
}

/// 渲染层使用的单条字幕（来源前缀已按 LabelMode 计算好）
#[derive(Debug, Clone, PartialEq)]
pub struct DisplayLine {
    pub source_id: String,
    /// None 表示本条不显示来源前缀
    pub label: Option<String>,
    pub source: String,
    pub translated: String,
    /// 桌面浮窗 / SteamVR 用的行文本（含前缀，可能多行）
    pub lines: Vec<String>,
    pub text: String,
}

pub struct SubtitleState {
    history: VecDeque<SubtitleLine>,
    max_lines: usize,
    show_source: bool,
    label_mode: LabelMode,
    /// 当前在跑的源标签（用于 Auto 判定）
    active_labels: Vec<String>,
}

impl SubtitleState {
    pub fn new(max_lines: usize, show_source: bool, label_mode: LabelMode) -> Self {
        Self {
            history: VecDeque::with_capacity(max_lines.max(1)),
            max_lines: max_lines.max(1),
            show_source,
            label_mode,
            active_labels: Vec::new(),
        }
    }

    /// 添加新字幕
    pub fn push(&mut self, line: SubtitleLine) {
        self.history.push_back(line);
        while self.history.len() > self.max_lines {
            self.history.pop_front();
        }
        tracing::debug!("Subtitle pushed. Current count: {}", self.history.len());
    }

    /// 当前应显示的条目（按时间从旧到新）
    pub fn display_lines(&self) -> Vec<DisplayLine> {
        self.history
            .iter()
            .map(|line| self.render_line(line))
            .collect()
    }

    fn render_line(&self, line: &SubtitleLine) -> DisplayLine {
        let label = if self.should_label() {
            Some(line.label.clone())
        } else {
            None
        };
        let prefix = label
            .as_ref()
            .map(|label| format!("[{label}] "))
            .unwrap_or_default();

        let mut lines = Vec::new();
        // 仅识别模式没有译文，直接显示原文
        if line.translated.trim().is_empty() {
            lines.push(format!("{prefix}{}", line.source));
        } else if self.show_source {
            // 只有译文、原文为空时不能推一行空的原文，否则每条字幕都会多一行空白
            if !line.source.trim().is_empty() {
                lines.push(format!("{prefix}{}", line.source));
            }
            lines.push(line.translated.clone());
        } else {
            lines.push(format!("{prefix}{}", line.translated));
        }

        DisplayLine {
            source_id: line.source_id.clone(),
            label,
            source: line.source.clone(),
            translated: line.translated.clone(),
            text: lines.join("\n"),
            lines,
        }
    }

    fn should_label(&self) -> bool {
        match self.label_mode {
            LabelMode::Always => true,
            LabelMode::Never => false,
            LabelMode::Auto => {
                if self.active_labels.len() >= 2 {
                    return true;
                }
                let mut first_source: Option<&str> = None;
                for item in &self.history {
                    match first_source {
                        None => first_source = Some(item.source_id.as_str()),
                        Some(first) if first != item.source_id => return true,
                        _ => {}
                    }
                }
                false
            }
        }
    }

    /// 当前所有字幕行（扁平化），供旧调用方使用
    #[allow(dead_code)]
    pub fn get_all(&self) -> Vec<String> {
        self.display_lines()
            .into_iter()
            .flat_map(|line| line.lines)
            .collect()
    }

    /// 获取合并后的字幕文本
    #[allow(dead_code)]
    pub fn get_text(&self) -> String {
        self.get_all().join("\n")
    }

    /// 当前条数与最大条数
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.history.len()
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.history.is_empty()
    }

    pub fn clear(&mut self) {
        self.history.clear();
    }

    pub fn set_show_source(&mut self, show_source: bool) {
        self.show_source = show_source;
        self.trim();
    }

    /// 调整历史上限。
    ///
    /// 这里**不裁剪**已有历史：设置页的实时预览会临时把它调到很小，
    /// 裁剪会让用户在点「取消」后永久丢掉那几行。真正的上限由 `push` 维持，
    /// 展示侧（浮窗 / OSC / SteamVR）各自按自己的 max_lines 裁剪。
    pub fn set_max_lines(&mut self, max_lines: usize) {
        self.max_lines = max_lines.max(1);
    }

    pub fn set_label_mode(&mut self, mode: LabelMode) {
        self.label_mode = mode;
    }

    /// 更新「正在运行的源」标签集合，影响 Auto 前缀判定
    pub fn set_active_labels(&mut self, labels: Vec<String>) {
        self.active_labels = labels;
    }

    fn trim(&mut self) {
        while self.history.len() > self.max_lines {
            self.history.pop_front();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(id: &str, label: &str, source: &str, translated: &str) -> SubtitleLine {
        SubtitleLine::new(id, label, source.into(), translated.into())
    }

    #[test]
    fn label_mode_auto_requires_two_active_sources() {
        let mut state = SubtitleState::new(3, false, LabelMode::Auto);
        state.push(line("mic", "麦克风", "hello", "你好"));

        state.set_active_labels(vec!["麦克风".into()]);
        assert_eq!(state.display_lines()[0].lines[0], "你好");

        state.set_active_labels(vec!["麦克风".into(), "游戏".into()]);
        assert_eq!(state.display_lines()[0].lines[0], "[麦克风] 你好");

        // 其中一路刚停止时，只要屏幕历史里仍混合多源字幕，就继续保留来源标识
        state.push(line("game", "游戏", "hi", "嗨"));
        state.set_active_labels(vec!["麦克风".into()]);
        assert_eq!(state.display_lines()[0].lines[0], "[麦克风] 你好");
        assert_eq!(state.display_lines()[1].lines[0], "[游戏] 嗨");
    }

    #[test]
    fn label_mode_always_and_never() {
        let mut always = SubtitleState::new(3, false, LabelMode::Always);
        always.push(line("mic", "麦克风", "hello", "你好"));
        assert_eq!(always.display_lines()[0].lines[0], "[麦克风] 你好");

        let mut never = SubtitleState::new(3, false, LabelMode::Never);
        never.push(line("mic", "麦克风", "hello", "你好"));
        never.set_active_labels(vec!["a".into(), "b".into()]);
        assert_eq!(never.display_lines()[0].lines[0], "你好");
    }

    /// 只有译文（原文被过滤/翻译-only 路径）时不能多出一行空白
    #[test]
    fn show_source_skips_empty_source_row() {
        let mut state = SubtitleState::new(3, true, LabelMode::Always);
        state.push(line("mic", "麦克风", "", "你好"));
        let entry = &state.display_lines()[0];
        assert_eq!(entry.lines, vec!["你好"]);
        assert_eq!(entry.text, "你好");
    }

    /// 设置页实时预览会把 max_lines 调小；取消后历史不能因此永久丢失
    #[test]
    fn lowering_max_lines_keeps_existing_history() {
        let mut state = SubtitleState::new(5, false, LabelMode::Never);
        for i in 0..5 {
            state.push(line("mic", "麦克风", &format!("line{i}"), ""));
        }
        state.set_max_lines(1);
        assert_eq!(state.display_lines().len(), 5);
        // 但下一次 push 会把上限收紧
        state.push(line("mic", "麦克风", "next", ""));
        assert_eq!(state.display_lines().len(), 1);
    }

    #[test]
    fn prefix_only_on_first_line() {
        let mut state = SubtitleState::new(3, true, LabelMode::Always);
        state.push(line("mic", "麦克风", "hello", "你好"));
        let entry = &state.display_lines()[0];
        assert_eq!(entry.lines, vec!["[麦克风] hello", "你好"]);
        assert_eq!(entry.text, "[麦克风] hello\n你好");
    }

    #[test]
    fn asr_only_falls_back_to_source() {
        let mut state = SubtitleState::new(3, false, LabelMode::Always);
        state.push(line("mic", "麦克风", "hello", ""));
        assert_eq!(state.display_lines()[0].lines, vec!["[麦克风] hello"]);
    }

    #[test]
    fn history_is_trimmed_to_max_lines() {
        let mut state = SubtitleState::new(2, false, LabelMode::Never);
        for i in 0..5 {
            state.push(line("mic", "麦", &format!("s{i}"), &format!("t{i}")));
        }
        let lines = state.display_lines();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text, "t3");
        assert_eq!(lines[1].text, "t4");

        // 调小上限不裁剪已有历史（设置页预览会临时调小），下一次 push 才收紧
        state.set_max_lines(1);
        assert_eq!(state.len(), 2);
        state.push(line("mic", "麦", "s5", "t5"));
        assert_eq!(state.len(), 1);
        assert_eq!(state.display_lines()[0].text, "t5");
    }

    #[test]
    fn clear_empties_history() {
        let mut state = SubtitleState::new(3, false, LabelMode::Auto);
        state.push(line("mic", "麦", "a", "b"));
        state.clear();
        assert!(state.is_empty());
        assert!(state.get_text().is_empty());
    }
}

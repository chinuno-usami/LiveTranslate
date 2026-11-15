use crate::config::{AppConfig, LabelMode, OutputChannel};
use std::collections::{HashSet, VecDeque};

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

/// 某个通道允许出现的 source_id 集合
#[derive(Debug, Clone)]
pub struct ChannelFilter {
    #[allow(dead_code)] // 诊断/日志保留通道身份；过滤只看 allowed
    pub channel: OutputChannel,
    pub allowed: HashSet<String>,
}

impl ChannelFilter {
    pub fn allows(&self, source_id: &str) -> bool {
        self.allowed.contains(source_id)
    }
}

/// 由配置构建某通道的源过滤器（只含当前配置里路由到该通道的源）
pub fn channel_filter(config: &AppConfig, channel: OutputChannel) -> ChannelFilter {
    let allowed = config
        .audio
        .effective_sources(config.translate.enabled)
        .into_iter()
        .filter(|s| s.routes_to(channel))
        .map(|s| s.id)
        .collect();
    ChannelFilter { channel, allowed }
}

/// 三个输出通道各自裁剪后的字幕视图
#[derive(Debug, Clone)]
pub struct SubtitleViews {
    pub desktop: Vec<DisplayLine>,
    pub osc: Vec<DisplayLine>,
    pub steamvr: Vec<DisplayLine>,
}

/// 保留末尾至多 `limit` 条（至少 1）
pub fn trim_last(mut entries: Vec<DisplayLine>, limit: usize) -> Vec<DisplayLine> {
    let limit = limit.max(1);
    if entries.len() > limit {
        entries.drain(..entries.len() - limit);
    }
    entries
}

/// 按通道路由生成桌面 / OSC / SteamVR 三份视图
pub fn subtitle_views_from(state: &SubtitleState, config: &AppConfig) -> SubtitleViews {
    SubtitleViews {
        desktop: trim_last(
            state.display_lines_for(&channel_filter(config, OutputChannel::Desktop)),
            config.subtitle.max_lines,
        ),
        osc: trim_last(
            state.display_lines_for(&channel_filter(config, OutputChannel::Osc)),
            config.output.osc.max_lines,
        ),
        steamvr: trim_last(
            state.display_lines_for(&channel_filter(config, OutputChannel::SteamVr)),
            config.output.steamvr.max_lines,
        ),
    }
}

pub struct SubtitleState {
    history: VecDeque<SubtitleLine>,
    max_lines: usize,
    show_source: bool,
    label_mode: LabelMode,
    /// 当前在跑的源 (id, label)，用于 Auto 判定
    active_sources: Vec<(String, String)>,
}

impl SubtitleState {
    pub fn new(max_lines: usize, show_source: bool, label_mode: LabelMode) -> Self {
        Self {
            history: VecDeque::with_capacity(max_lines.max(1)),
            max_lines: max_lines.max(1),
            show_source,
            label_mode,
            active_sources: Vec::new(),
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

    /// 未按通道过滤的全量视图（兼容旧调用；Auto 看全部运行源）
    pub fn display_lines(&self) -> Vec<DisplayLine> {
        let should_label = self.should_label_unfiltered();
        self.history
            .iter()
            .map(|line| self.render_line(line, should_label))
            .collect()
    }

    /// 只渲染该通道允许的源，并按该通道的可见源重新判定 Auto 标签。
    pub fn display_lines_for(&self, filter: &ChannelFilter) -> Vec<DisplayLine> {
        let visible: Vec<&SubtitleLine> = self
            .history
            .iter()
            .filter(|line| filter.allows(&line.source_id))
            .collect();
        let should_label = self.should_label_for(filter, &visible);
        visible
            .into_iter()
            .map(|line| self.render_line(line, should_label))
            .collect()
    }

    fn render_line(&self, line: &SubtitleLine, should_label: bool) -> DisplayLine {
        let label = if should_label {
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
            // 只有译文、原文为空时不能推一行空的原文，否则每条字幕都会多一行空白；
            // 此时译文成为首行，需补上 [来源] 前缀。
            if line.source.trim().is_empty() {
                lines.push(format!("{prefix}{}", line.translated));
            } else {
                lines.push(format!("{prefix}{}", line.source));
                lines.push(line.translated.clone());
            }
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

    fn should_label_unfiltered(&self) -> bool {
        match self.label_mode {
            LabelMode::Always => true,
            LabelMode::Never => false,
            LabelMode::Auto => {
                if self.active_sources.len() >= 2 {
                    return true;
                }
                history_has_multiple_source_ids(self.history.iter())
            }
        }
    }

    fn should_label_for(&self, filter: &ChannelFilter, visible: &[&SubtitleLine]) -> bool {
        match self.label_mode {
            LabelMode::Always => true,
            LabelMode::Never => false,
            LabelMode::Auto => {
                // 仅统计「该通道可见且正在运行」的源
                let active_in_channel = self
                    .active_sources
                    .iter()
                    .filter(|(id, _)| filter.allows(id))
                    .count();
                if active_in_channel >= 2 {
                    return true;
                }
                // 第二条件也只看通道内可见历史，不可见源不触发前缀
                history_has_multiple_source_ids(visible.iter().copied())
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
    }

    /// 显式按当前 `max_lines` 裁剪历史（正式保存配置时调用；预览期间不调用以免丢历史）
    pub fn trim_to_limit(&mut self) {
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

    /// 更新「正在运行的源」集合，影响 Auto 前缀判定
    pub fn set_active_sources(&mut self, sources: Vec<(String, String)>) {
        self.active_sources = sources;
    }

    fn trim(&mut self) {
        while self.history.len() > self.max_lines {
            self.history.pop_front();
        }
    }
}

fn history_has_multiple_source_ids<'a>(items: impl IntoIterator<Item = &'a SubtitleLine>) -> bool {
    let mut first_source: Option<&str> = None;
    for item in items {
        match first_source {
            None => first_source = Some(item.source_id.as_str()),
            Some(first) if first != item.source_id => return true,
            _ => {}
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AudioSourceConfig;

    fn line(id: &str, label: &str, source: &str, translated: &str) -> SubtitleLine {
        SubtitleLine::new(id, label, source.into(), translated.into())
    }

    fn filter(channel: OutputChannel, ids: &[&str]) -> ChannelFilter {
        ChannelFilter {
            channel,
            allowed: ids.iter().map(|id| (*id).to_string()).collect(),
        }
    }

    #[test]
    fn label_mode_auto_requires_two_active_sources() {
        let mut state = SubtitleState::new(3, false, LabelMode::Auto);
        state.push(line("mic", "麦克风", "hello", "你好"));

        state.set_active_sources(vec![("mic".into(), "麦克风".into())]);
        assert_eq!(state.display_lines()[0].lines[0], "你好");

        state.set_active_sources(vec![
            ("mic".into(), "麦克风".into()),
            ("game".into(), "游戏".into()),
        ]);
        assert_eq!(state.display_lines()[0].lines[0], "[麦克风] 你好");

        // 其中一路刚停止时，只要屏幕历史里仍混合多源字幕，就继续保留来源标识
        state.push(line("game", "游戏", "hi", "嗨"));
        state.set_active_sources(vec![("mic".into(), "麦克风".into())]);
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
        never.set_active_sources(vec![("a".into(), "a".into()), ("b".into(), "b".into())]);
        assert_eq!(never.display_lines()[0].lines[0], "你好");
    }

    /// 只有译文（原文被过滤/翻译-only 路径）时不能多出一行空白，且保留来源前缀
    #[test]
    fn show_source_skips_empty_source_row() {
        let mut state = SubtitleState::new(3, true, LabelMode::Always);
        state.push(line("mic", "麦克风", "", "你好"));
        let entry = &state.display_lines()[0];
        assert_eq!(entry.lines, vec!["[麦克风] 你好"]);
        assert_eq!(entry.text, "[麦克风] 你好");
    }

    /// 设置页实时预览同时调用 set_max_lines + set_show_source 时不能把旧字幕裁掉
    #[test]
    fn preview_set_max_lines_and_show_source_preserves_history() {
        let mut state = SubtitleState::new(5, false, LabelMode::Never);
        for i in 0..5 {
            state.push(line("mic", "麦克风", &format!("s{i}"), &format!("t{i}")));
        }
        state.set_max_lines(1);
        state.set_show_source(true);
        assert_eq!(state.len(), 5);
        // 撤销预览恢复上限后，5 条历史完整保留
        state.set_max_lines(5);
        state.set_show_source(false);
        assert_eq!(state.display_lines().len(), 5);
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

    #[test]
    fn display_lines_for_desktop_shows_both_with_auto_prefix() {
        let mut state = SubtitleState::new(10, false, LabelMode::Auto);
        state.set_active_sources(vec![("a".into(), "A".into()), ("b".into(), "B".into())]);
        state.push(line("a", "A", "sa", "ta"));
        state.push(line("b", "B", "sb", "tb"));
        let desktop = filter(OutputChannel::Desktop, &["a", "b"]);
        let lines = state.display_lines_for(&desktop);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].lines[0], "[A] ta");
        assert_eq!(lines[1].lines[0], "[B] tb");
    }

    #[test]
    fn display_lines_for_osc_single_visible_drops_auto_prefix() {
        let mut state = SubtitleState::new(10, false, LabelMode::Auto);
        state.set_active_sources(vec![("a".into(), "A".into()), ("b".into(), "B".into())]);
        state.push(line("a", "A", "sa", "ta"));
        state.push(line("b", "B", "sb", "tb"));
        let osc = filter(OutputChannel::Osc, &["a"]);
        let lines = state.display_lines_for(&osc);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].source_id, "a");
        assert_eq!(lines[0].lines[0], "ta");
    }

    #[test]
    fn display_lines_for_hides_disallowed_sources() {
        let mut state = SubtitleState::new(10, false, LabelMode::Never);
        state.push(line("a", "A", "sa", "ta"));
        state.push(line("b", "B", "sb", "tb"));
        state.push(line("c", "C", "sc", "tc"));
        let lines = state.display_lines_for(&filter(OutputChannel::SteamVr, &["b"]));
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].source_id, "b");
    }

    #[test]
    fn display_lines_for_always_keeps_prefix_with_single_visible() {
        let mut state = SubtitleState::new(10, false, LabelMode::Always);
        state.set_active_sources(vec![("a".into(), "A".into())]);
        state.push(line("a", "A", "sa", "ta"));
        let lines = state.display_lines_for(&filter(OutputChannel::Desktop, &["a"]));
        assert_eq!(lines[0].lines[0], "[A] ta");
    }

    #[test]
    fn display_lines_for_auto_ignores_hidden_history_sources() {
        // 历史混合 A/B，当前通道只允许 A → 不因历史里的 B 触发 Auto 前缀
        let mut state = SubtitleState::new(10, false, LabelMode::Auto);
        state.set_active_sources(vec![("a".into(), "A".into())]);
        state.push(line("a", "A", "sa", "ta"));
        state.push(line("b", "B", "sb", "tb"));
        let lines = state.display_lines_for(&filter(OutputChannel::Osc, &["a"]));
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].lines[0], "ta");
    }

    #[test]
    fn trim_last_and_subtitle_views_respect_per_channel_limits() {
        let mut state = SubtitleState::new(10, false, LabelMode::Never);
        state.set_active_sources(vec![("a".into(), "A".into())]);
        for i in 0..5 {
            state.push(SubtitleLine::new(
                "a",
                "A",
                format!("s{i}"),
                format!("t{i}"),
            ));
        }
        let mut config = AppConfig::default();
        config.audio.sources = vec![AudioSourceConfig::new("a", "A", "mic:0")];
        config.subtitle.max_lines = 2;
        config.output.osc.max_lines = 3;
        config.output.steamvr.max_lines = 4;
        let views = subtitle_views_from(&state, &config);
        assert_eq!(views.desktop.len(), 2);
        assert_eq!(views.osc.len(), 3);
        assert_eq!(views.steamvr.len(), 4);
        assert_eq!(views.desktop[0].translated, "t3");
        assert_eq!(views.osc[0].translated, "t2");
        assert_eq!(views.steamvr[0].translated, "t1");
    }

    #[test]
    fn channel_filter_uses_routing_flags() {
        let mut config = AppConfig::default();
        config.audio.sources = vec![
            AudioSourceConfig {
                id: "mic".into(),
                label: "麦克风".into(),
                device: "mic".into(),
                enabled: true,
                translate: true,
                language: String::new(),
                desktop: true,
                osc: true,
                steamvr: false,
            },
            AudioSourceConfig {
                id: "game".into(),
                label: "游戏".into(),
                device: "loopback".into(),
                enabled: true,
                translate: true,
                language: String::new(),
                desktop: true,
                osc: false,
                steamvr: true,
            },
        ];
        let desktop = channel_filter(&config, OutputChannel::Desktop);
        assert!(desktop.allows("mic") && desktop.allows("game"));
        let osc = channel_filter(&config, OutputChannel::Osc);
        assert!(osc.allows("mic") && !osc.allows("game"));
        let vr = channel_filter(&config, OutputChannel::SteamVr);
        assert!(!vr.allows("mic") && vr.allows("game"));
    }

    #[test]
    fn routes_to_matches_channel_flags() {
        let src = AudioSourceConfig {
            id: "x".into(),
            label: "X".into(),
            device: "mic".into(),
            enabled: true,
            translate: true,
            language: String::new(),
            desktop: true,
            osc: false,
            steamvr: true,
        };
        assert!(src.routes_to(OutputChannel::Desktop));
        assert!(!src.routes_to(OutputChannel::Osc));
        assert!(src.routes_to(OutputChannel::SteamVr));
    }
}

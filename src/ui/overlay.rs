use crate::config::SubtitleConfig;
use crate::subtitle::DisplayLine;
use serde::Serialize;

pub const SUBTITLE_EVENT: &str = "subtitle://update";
pub const STATUS_EVENT: &str = "status://update";
pub const SOURCE_STATUS_EVENT: &str = "source-status://update";
pub const OUTPUT_STATUS_EVENT: &str = "output-status://update";
pub const CONFIG_EVENT: &str = "config://subtitle";
pub const ERROR_EVENT: &str = "error://message";
pub const NOTICE_EVENT: &str = "notice://message";
pub const CLICK_THROUGH_EVENT: &str = "click-through://update";

#[derive(Debug, Clone, Serialize)]
pub struct SubtitleEntryPayload {
    pub source_id: String,
    pub label: String,
    pub source: String,
    pub translated: String,
    pub lines: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ActiveSourcePayload {
    pub id: String,
    pub label: String,
    pub translate: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SubtitlePayload {
    pub entries: Vec<SubtitleEntryPayload>,
    pub text: String,
    pub active_sources: Vec<ActiveSourcePayload>,
}

impl SubtitlePayload {
    pub fn new(entries: &[DisplayLine], active_sources: Vec<ActiveSourcePayload>) -> Self {
        Self {
            entries: entries
                .iter()
                .map(|line| SubtitleEntryPayload {
                    source_id: line.source_id.clone(),
                    label: line.label.clone().unwrap_or_default(),
                    source: line.source.clone(),
                    translated: line.translated.clone(),
                    lines: line.lines.clone(),
                })
                .collect(),
            text: entries
                .iter()
                .map(|line| line.text.clone())
                .collect::<Vec<_>>()
                .join("\n"),
            active_sources,
        }
    }

    pub fn empty() -> Self {
        Self {
            entries: Vec::new(),
            text: String::new(),
            active_sources: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct StatusPayload {
    pub running: bool,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SourceStatusItem {
    pub id: String,
    pub label: String,
    pub running: bool,
    pub enabled: bool,
    pub translate: bool,
    pub device: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SourcesPayload {
    pub sources: Vec<SourceStatusItem>,
    pub max_active_sources: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct OverlayConfigPayload {
    pub font_size: u32,
    pub text_color: String,
    pub stroke_color: String,
    pub background: String,
    pub toolbar_background: String,
    pub show_source: bool,
    pub window_width: u32,
    pub window_height: u32,
    pub click_through: bool,
    /// false 为仅识别模式
    pub translate_enabled: bool,
    pub max_lines: usize,
    pub label_sources: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DevicePayload {
    pub spec: String,
    pub name: String,
    pub kind: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DevicesPayload {
    pub devices: Vec<DevicePayload>,
    pub current: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct LanguageOptionPayload {
    pub code: String,
    pub label: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct LanguagesPayload {
    pub options: Vec<LanguageOptionPayload>,
    pub current: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CapabilitiesPayload {
    pub steamvr: bool,
    pub osc: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SaveResult {
    pub applied: usize,
    pub restart_required: bool,
    pub message: String,
}

impl OverlayConfigPayload {
    pub fn new(value: &SubtitleConfig, translate_enabled: bool) -> Self {
        Self {
            font_size: value.font_size,
            text_color: value.text_color.clone(),
            stroke_color: value.stroke_color.clone(),
            background: value.background.clone(),
            toolbar_background: value.toolbar_background.clone(),
            show_source: value.show_source,
            window_width: value.window_width,
            window_height: value.window_height,
            click_through: value.click_through,
            translate_enabled,
            max_lines: value.max_lines,
            label_sources: value.label_sources.as_str().to_string(),
        }
    }
}

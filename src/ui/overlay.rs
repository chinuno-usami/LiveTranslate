use crate::config::SubtitleConfig;
use serde::Serialize;

pub const SUBTITLE_EVENT: &str = "subtitle://update";
pub const STATUS_EVENT: &str = "status://update";
pub const CONFIG_EVENT: &str = "config://subtitle";
pub const ERROR_EVENT: &str = "error://message";
pub const CLICK_THROUGH_EVENT: &str = "click-through://update";

#[derive(Debug, Clone, Serialize)]
pub struct SubtitlePayload {
    pub source: String,
    pub translated: String,
    pub lines: Vec<String>,
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct StatusPayload {
    pub running: bool,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct OverlayConfigPayload {
    pub font_size: u32,
    pub text_color: String,
    pub stroke_color: String,
    pub show_source: bool,
    pub window_width: u32,
    pub window_height: u32,
    pub click_through: bool,
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

impl From<&SubtitleConfig> for OverlayConfigPayload {
    fn from(value: &SubtitleConfig) -> Self {
        Self {
            font_size: value.font_size,
            text_color: value.text_color.clone(),
            stroke_color: value.stroke_color.clone(),
            show_source: value.show_source,
            window_width: value.window_width,
            window_height: value.window_height,
            click_through: value.click_through,
        }
    }
}

//! 设置页字段 schema（后端驱动）。
//!
//! 好处是设置页不需要硬编码字段列表；新增配置项时只改这里，
//! 单测会校验每个 path 都能映射回 [`AppConfig`]，避免结构体改名后静默失效。

use crate::config::AppConfig;
use serde::Serialize;
use serde_json::Value as JsonValue;

#[derive(Debug, Clone, Serialize)]
pub struct SettingsSchema {
    pub groups: Vec<SettingsGroup>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SettingsGroup {
    pub id: String,
    pub title: String,
    pub description: String,
    pub fields: Vec<SettingsField>,
    /// 自定义组件标记："sources_table" | "raw_toml"
    pub custom: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldKind {
    Bool,
    Int,
    Float,
    String,
    Secret,
    Enum,
    Multiline,
    Toml,
    Color,
    Device,
    Language,
}

#[derive(Debug, Clone, Serialize)]
pub struct FieldOption {
    pub value: String,
    pub label: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SettingsField {
    pub path: String,
    pub label: String,
    pub help: String,
    pub kind: FieldKind,
    pub value: JsonValue,
    pub options: Vec<FieldOption>,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub step: Option<f64>,
    pub restart_required: bool,
    pub advanced: bool,
}

/// 生成设置页 schema（值取自当前配置）
pub fn build(config: &AppConfig) -> SettingsSchema {
    let groups = vec![
        SettingsGroup {
            id: "sources".into(),
            title: "输入源".into(),
            description: "同时采集多个麦克风 / 系统回环；每个源可单独开关翻译与识别语言。".into(),
            custom: Some("sources_table".into()),
            fields: vec![
                device(
                    config,
                    "audio.device_name",
                    "默认设备（旧字段）",
                    "仅作为兼容字段保留；多源配置以输入源表格为准。",
                ),
                int(
                    config,
                    "audio.max_active_sources",
                    "同时采集上限",
                    "超过上限的源不会启动（按列表顺序取前 N 个）。",
                    1.0,
                    8.0,
                ),
                float(
                    config,
                    "audio.chunk_seconds",
                    "固定切片时长（秒）",
                    "关闭 VAD 后按固定时长切分。",
                    0.5,
                    10.0,
                    0.5,
                ),
                int(
                    config,
                    "audio.sample_rate",
                    "目标采样率",
                    "Whisper 推荐 16000。",
                    8000.0,
                    48000.0,
                ),
                int(
                    config,
                    "audio.channels",
                    "声道数",
                    "内部固定下混为单声道。",
                    1.0,
                    2.0,
                ),
                float(
                    config,
                    "audio.silence_threshold",
                    "静音阈值",
                    "仅在关闭 VAD 时生效（0–1）。",
                    0.0,
                    1.0,
                    0.01,
                ),
            ],
        },
        SettingsGroup {
            id: "vad".into(),
            title: "语音分段 (VAD)".into(),
            description: "按语音边界切句。修改后需要重启采集。".into(),
            custom: None,
            fields: vec![
                bool_(
                    config,
                    "vad.enabled",
                    "启用 VAD",
                    "关闭后回退为固定时长切片。",
                ),
                enum_(
                    config,
                    "vad.backend",
                    "VAD 后端",
                    "energy 无额外依赖；silero 需 ONNX Runtime，能区分音乐与人声。",
                    vec![("energy", "能量型"), ("silero", "Silero")],
                ),
                int(
                    config,
                    "vad.frame_ms",
                    "帧长（毫秒）",
                    "仅 energy 后端；silero 固定 512 样本。",
                    10.0,
                    100.0,
                ),
                float(
                    config,
                    "vad.margin_db",
                    "噪声余量 (dB)",
                    "环境嘈杂调大。",
                    0.0,
                    40.0,
                    0.5,
                ),
                float(
                    config,
                    "vad.noise_percentile",
                    "噪声底分位",
                    "0–1，仅 energy 后端。",
                    0.0,
                    1.0,
                    0.01,
                ),
                string(
                    config,
                    "vad.silero_model",
                    "Silero 模型路径",
                    "留空使用内嵌模型。",
                ),
                float(
                    config,
                    "vad.silero_threshold",
                    "Silero 阈值",
                    "0–1。",
                    0.0,
                    1.0,
                    0.01,
                ),
                int(
                    config,
                    "vad.min_speech_ms",
                    "最短语音（毫秒）",
                    "短于此的片段被丢弃。",
                    20.0,
                    10000.0,
                ),
                int(
                    config,
                    "vad.min_silence_ms",
                    "句尾静音（毫秒）",
                    "静音持续这么久才确认说完。",
                    50.0,
                    5000.0,
                ),
                int(
                    config,
                    "vad.max_speech_ms",
                    "单段最长（毫秒）",
                    "超过则强制切分。",
                    500.0,
                    30000.0,
                ),
                int_adv(
                    config,
                    "vad.pre_pad_ms",
                    "前置保留（毫秒）",
                    "避免吃掉首音素。",
                    0.0,
                    2000.0,
                ),
                int_adv(
                    config,
                    "vad.post_pad_ms",
                    "后置保留（毫秒）",
                    "避免吃掉尾音素。",
                    0.0,
                    2000.0,
                ),
            ],
        },
        SettingsGroup {
            id: "asr".into(),
            title: "语音识别".into(),
            description: "识别后端与连接参数。除语言外，修改后需要重启采集。".into(),
            custom: None,
            fields: vec![
                enum_(
                    config,
                    "asr.backend",
                    "识别后端",
                    "whisper 为自建/兼容服务；edge 为微软在线识别。",
                    vec![("whisper", "Whisper 兼容服务"), ("edge", "Edge 在线识别")],
                ),
                string(
                    config,
                    "asr.base_url",
                    "服务地址",
                    "Whisper 兼容 API 基础 URL。",
                ),
                string(config, "asr.model", "模型名", "例如 whisper-1。"),
                language(
                    config,
                    "asr.language",
                    "识别语言",
                    "auto 为自动检测；也可在浮窗下拉框切换（立即生效）。",
                    "whisper",
                ),
                int(config, "asr.timeout_secs", "超时（秒）", "", 3.0, 300.0),
                string(
                    config,
                    "asr.request_path",
                    "请求路径",
                    "例如 /v1/audio/transcriptions。",
                ),
                secret(
                    config,
                    "asr.api_key",
                    "访问令牌",
                    "可选，留空不发送认证头。",
                ),
                string(
                    config,
                    "asr.auth_header",
                    "认证头名称",
                    "默认 Authorization；可改为 api-key 等。",
                ),
                bool_(
                    config,
                    "asr.filter_hallucination",
                    "过滤幻听",
                    "丢弃音乐/噪声引起的固定幻听文本。",
                ),
                language(
                    config,
                    "asr.edge.language",
                    "Edge 语言",
                    "BCP-47，如 en-US / zh-CN（不能用 auto）。",
                    "edge",
                ),
                int(
                    config,
                    "asr.edge.timeout_secs",
                    "Edge 超时（秒）",
                    "",
                    3.0,
                    120.0,
                ),
                secret_adv(
                    config,
                    "asr.edge.trusted_client_token",
                    "Edge 身份令牌",
                    "服务端可能轮换，一般无需修改。",
                ),
                string_adv(
                    config,
                    "asr.edge.chromium_full_version",
                    "Edge Chromium 版本",
                    "一般无需修改。",
                ),
                string_adv(config, "asr.edge.origin", "Edge Origin", "一般无需修改。"),
            ],
        },
        SettingsGroup {
            id: "translate".into(),
            title: "翻译".into(),
            description: "OpenAI 兼容翻译服务。修改连接参数后需要重启采集。".into(),
            custom: None,
            fields: vec![
                bool_(
                    config,
                    "translate.enabled",
                    "启用翻译",
                    "关闭后为仅识别模式（立即生效）。",
                ),
                string(
                    config,
                    "translate.base_url",
                    "服务地址",
                    "例如 https://api.deepseek.com/v1。",
                ),
                secret(
                    config,
                    "translate.api_key",
                    "API Key",
                    "日志中始终脱敏显示。",
                ),
                string(config, "translate.model", "模型", ""),
                string(
                    config,
                    "translate.target_language",
                    "目标语言",
                    "例如 zh-CN。",
                ),
                multiline(config, "translate.system_prompt", "系统提示词", ""),
                int(
                    config,
                    "translate.timeout_secs",
                    "超时（秒）",
                    "",
                    3.0,
                    300.0,
                ),
                bool_(
                    config,
                    "translate.send_temperature",
                    "发送 temperature",
                    "部分推理模型不接受该字段。",
                ),
                float(
                    config,
                    "translate.temperature",
                    "temperature",
                    "仅在上方开启时发送。",
                    0.0,
                    2.0,
                    0.05,
                ),
                enum_(
                    config,
                    "translate.disable_thinking",
                    "关闭思考策略",
                    "各家字段名不同；不确定时用 extra_body。",
                    vec![
                        ("", "不处理"),
                        ("reasoning_effort", "reasoning_effort"),
                        ("enable_thinking", "enable_thinking"),
                        ("chat_template", "chat_template"),
                    ],
                ),
                toml(
                    config,
                    "translate.extra_body",
                    "extra_body",
                    "追加到请求体的 TOML 表，优先级最高。例：thinking = { type = \"disabled\" }",
                ),
            ],
        },
        SettingsGroup {
            id: "subtitle".into(),
            title: "字幕浮窗".into(),
            description: "桌面浮窗的样式与几何；修改立即生效。".into(),
            custom: None,
            fields: vec![
                int(
                    config,
                    "subtitle.max_lines",
                    "最多显示条数",
                    "浮窗保留的最近字幕条数。",
                    1.0,
                    10.0,
                ),
                int(
                    config,
                    "subtitle.font_size",
                    "字号",
                    "12–72 px。",
                    12.0,
                    72.0,
                ),
                color(config, "subtitle.text_color", "文字颜色", "任意 CSS 颜色。"),
                color(
                    config,
                    "subtitle.stroke_color",
                    "描边颜色",
                    "任意 CSS 颜色。",
                ),
                string(
                    config,
                    "subtitle.background",
                    "面板背景",
                    "任意 CSS 颜色，含透明度。",
                ),
                string(
                    config,
                    "subtitle.toolbar_background",
                    "工具栏背景",
                    "任意 CSS 颜色，含透明度。",
                ),
                bool_(
                    config,
                    "subtitle.show_source",
                    "显示原文",
                    "在译文上方显示识别原文。",
                ),
                enum_(
                    config,
                    "subtitle.label_sources",
                    "来源标识",
                    "多源时是否在字幕前加 [来源] 前缀。",
                    vec![
                        ("auto", "自动（≥2 源）"),
                        ("always", "始终"),
                        ("never", "从不"),
                    ],
                ),
                bool_adv(config, "subtitle.always_on_top", "窗口置顶", ""),
                bool_adv(
                    config,
                    "subtitle.click_through",
                    "点击穿透",
                    "开启后浮窗不接收鼠标事件，用托盘/快捷键关闭。",
                ),
                int_adv(
                    config,
                    "subtitle.window_width",
                    "窗口宽度",
                    "逻辑像素。",
                    160.0,
                    7680.0,
                ),
                int_adv(
                    config,
                    "subtitle.window_height",
                    "窗口高度",
                    "逻辑像素。",
                    80.0,
                    4320.0,
                ),
                int_adv(
                    config,
                    "subtitle.position_x",
                    "窗口 X",
                    "",
                    -20000.0,
                    20000.0,
                ),
                int_adv(
                    config,
                    "subtitle.position_y",
                    "窗口 Y",
                    "",
                    -20000.0,
                    20000.0,
                ),
            ],
        },
        SettingsGroup {
            id: "steamvr".into(),
            title: "SteamVR Overlay".into(),
            description: "在 VR 头显中显示字幕面板；仅 Windows Full 版可用。".into(),
            custom: None,
            fields: vec![
                bool_(
                    config,
                    "output.steamvr.enabled",
                    "启用 SteamVR 输出",
                    "需要 Full 版构建 + SteamVR 运行时。",
                ),
                enum_(
                    config,
                    "output.steamvr.anchor",
                    "锚点",
                    "hmd 跟随头部；world 固定在世界坐标。",
                    vec![("hmd", "跟随头部"), ("world", "世界坐标")],
                ),
                float(
                    config,
                    "output.steamvr.width_m",
                    "面板宽度（米）",
                    "",
                    0.3,
                    5.0,
                    0.05,
                ),
                float(
                    config,
                    "output.steamvr.distance_m",
                    "前方距离（米）",
                    "hmd 模式。",
                    0.2,
                    10.0,
                    0.05,
                ),
                float(
                    config,
                    "output.steamvr.height_offset_m",
                    "高度偏移（米）",
                    "负数偏下。",
                    -2.0,
                    2.0,
                    0.01,
                ),
                int(
                    config,
                    "output.steamvr.max_lines",
                    "最多显示条数",
                    "",
                    1.0,
                    8.0,
                ),
                bool_(config, "output.steamvr.show_source", "显示原文", ""),
                int(
                    config,
                    "output.steamvr.font_size_px",
                    "字号（px）",
                    "以 1024px 纹理宽为基准。",
                    12.0,
                    96.0,
                ),
                color(
                    config,
                    "output.steamvr.background",
                    "面板背景",
                    "#RRGGBBAA。",
                ),
                color(config, "output.steamvr.text_color", "译文颜色", ""),
                color(config, "output.steamvr.source_color", "原文颜色", ""),
                color(config, "output.steamvr.label_color", "来源标识颜色", ""),
                float(
                    config,
                    "output.steamvr.opacity",
                    "整块不透明度",
                    "0–1。",
                    0.0,
                    1.0,
                    0.01,
                ),
                float(
                    config,
                    "output.steamvr.curvature",
                    "曲率",
                    "0 平面；0.2–0.4 轻微曲面。",
                    0.0,
                    1.0,
                    0.05,
                ),
                int_adv(
                    config,
                    "output.steamvr.hide_on_silence_secs",
                    "静音后隐藏（秒）",
                    "0 表示不自动隐藏。",
                    0.0,
                    600.0,
                ),
                bool_adv(
                    config,
                    "output.steamvr.swap_red_blue",
                    "交换红蓝通道",
                    "颜色反了时打开。",
                ),
                string_adv(
                    config,
                    "output.steamvr.font_path",
                    "字体文件",
                    "留空自动使用系统中文字体。",
                ),
                int_adv(
                    config,
                    "output.steamvr.reconnect_interval_secs",
                    "重连间隔（秒）",
                    "SteamVR 未启动时的重试间隔。",
                    1.0,
                    60.0,
                ),
                float_adv(
                    config,
                    "output.steamvr.world_position[]",
                    "世界坐标",
                    "三个数：x, y, z（米）。",
                    -100.0,
                    100.0,
                ),
                float_adv(
                    config,
                    "output.steamvr.world_rotation_deg[]",
                    "世界旋转（度）",
                    "三个数：x, y, z（当前仅应用绕 Y 轴偏航角 y）。",
                    -360.0,
                    360.0,
                ),
                float_adv(
                    config,
                    "output.steamvr.line_alphas[]",
                    "逐行不透明度",
                    "三个数（0–1）：最新一行到更早行的淡出系数。",
                    0.0,
                    1.0,
                ),
            ],
        },
        SettingsGroup {
            id: "osc".into(),
            title: "OSC / VRChat".into(),
            description: "把字幕发送到 VRChat Chatbox；仅 Full 版可用。".into(),
            custom: None,
            fields: vec![
                bool_(
                    config,
                    "output.osc.enabled",
                    "启用 OSC 输出",
                    "需要 Full 版构建；VRChat 需开启 OSC。",
                ),
                string(
                    config,
                    "output.osc.target",
                    "目标地址",
                    "默认 127.0.0.1:9000。跨机发送会暴露字幕内容。",
                ),
                enum_(
                    config,
                    "output.osc.content",
                    "发送内容",
                    "",
                    vec![
                        ("translated", "仅译文"),
                        ("source", "仅原文"),
                        ("both", "原文 + 译文"),
                    ],
                ),
                enum_(
                    config,
                    "output.osc.mode",
                    "组包模式",
                    "rolling 合并最近多条为滚动字幕；latest 只发最新一条。",
                    vec![("rolling", "滚动合并"), ("latest", "仅最新")],
                ),
                int(
                    config,
                    "output.osc.max_lines",
                    "最多合并条数",
                    "VRChat 最多显示 9 行。",
                    1.0,
                    9.0,
                ),
                int(
                    config,
                    "output.osc.max_chars",
                    "字符预算",
                    "VRChat Chatbox 上限 144。",
                    8.0,
                    144.0,
                ),
                string(
                    config,
                    "output.osc.separator",
                    "行分隔符",
                    "默认 \\n（换行）；换行被过滤时可改为 \" | \"。",
                ),
                int(
                    config,
                    "output.osc.min_interval_ms",
                    "最小发送间隔（毫秒）",
                    "保守限流，避免触发 VRChat 冷却。",
                    200.0,
                    10000.0,
                ),
                int(
                    config,
                    "output.osc.max_per_window",
                    "5 秒内最多发送",
                    "",
                    1.0,
                    10.0,
                ),
                bool_(
                    config,
                    "output.osc.send_typing",
                    "发送 typing 指示",
                    "采集期间显示「正在输入」。",
                ),
                string_adv(
                    config,
                    "output.osc.custom_address",
                    "额外接收端（host:port）",
                    "可选；向额外 UDP 目标同步发送 (text, source_id, label)。",
                ),
                bool_adv(config, "output.osc.debug_log", "记录组包日志", "调试用。"),
            ],
        },
        SettingsGroup {
            id: "advanced".into(),
            title: "高级 / 原始配置".into(),
            description: "直接编辑 TOML。保存前会完整校验，非法内容不会落盘。".into(),
            custom: Some("raw_toml".into()),
            fields: vec![],
        },
    ];

    SettingsSchema {
        groups,
        warning: None,
    }
}

pub fn build_with_warning(config: &AppConfig, warning: Option<String>) -> SettingsSchema {
    let mut schema = build(config);
    schema.warning = warning;
    schema
}

/// 该字段是否需要重启采集才能生效
pub fn is_restart_required(path: &str) -> bool {
    if path.starts_with("output.") || path.starts_with("subtitle.") {
        return false;
    }
    if path == "translate.enabled" || path == "asr.language" || path == "asr.edge.language" {
        return false;
    }
    if path.starts_with("audio.") || path.starts_with("vad.") || path.starts_with("translate.") {
        return true;
    }
    path.starts_with("asr.")
}

/// 把设置页提交的单个值写入配置。
///
/// 返回 `restart_required`。
pub fn apply_change(config: &mut AppConfig, path: &str, value: &JsonValue) -> Result<bool, String> {
    let mut root = toml::Value::try_from(&*config).map_err(|e| format!("序列化配置失败: {e}"))?;
    let key_path = normalize_path(path);
    let toml_value = json_to_toml(value, path)?;
    set_toml_path(&mut root, &key_path, toml_value)?;
    let text = toml::to_string(&root).map_err(|e| format!("序列化配置失败: {e}"))?;
    *config = toml::from_str(&text)
        .map_err(|e| format!("配置类型不匹配: {}", crate::config::toml_error_text(&e)))?;
    Ok(is_restart_required(path))
}

/// 读取配置中某个 path 的值（用于 schema / 测试）
pub fn get_value(config: &AppConfig, path: &str) -> Option<JsonValue> {
    let root = toml::Value::try_from(config).ok()?;
    let mut current = &root;
    for part in path.trim_end_matches("[]").split('.') {
        current = current.get(part)?;
    }
    toml_to_json(current)
}

fn normalize_path(path: &str) -> Vec<String> {
    path.trim_end_matches("[]")
        .split('.')
        .map(str::to_string)
        .collect()
}

fn set_toml_path(
    root: &mut toml::Value,
    parts: &[String],
    value: toml::Value,
) -> Result<(), String> {
    if parts.is_empty() {
        return Err("配置路径为空".to_string());
    }
    let mut current = root;
    for part in &parts[..parts.len() - 1] {
        if !current.is_table() {
            return Err(format!("配置段 {part} 不是表"));
        }
        let table = current.as_table_mut().unwrap();
        current = table
            .entry(part.clone())
            .or_insert_with(|| toml::Value::Table(Default::default()));
    }
    let table = current
        .as_table_mut()
        .ok_or_else(|| "无法写入配置".to_string())?;
    table.insert(parts[parts.len() - 1].clone(), value);
    Ok(())
}

fn json_to_toml(value: &JsonValue, path: &str) -> Result<toml::Value, String> {
    Ok(match value {
        JsonValue::Bool(v) => toml::Value::Boolean(*v),
        JsonValue::Number(n) => {
            if let Some(i) = n.as_i64() {
                toml::Value::Integer(i)
            } else if let Some(f) = n.as_f64() {
                toml::Value::Float(f)
            } else {
                return Err(format!("{path}: 不支持的数字"));
            }
        }
        JsonValue::String(s) if path == "translate.extra_body" => {
            if s.trim().is_empty() {
                return Ok(toml::Value::Table(Default::default()));
            }
            let parsed: toml::Value = toml::from_str(s).map_err(|e| {
                format!(
                    "extra_body TOML 解析失败: {}",
                    crate::config::toml_error_text(&e)
                )
            })?;
            if !parsed.is_table() {
                return Err("translate.extra_body 必须是 TOML 表结构".to_string());
            }
            parsed
        }
        JsonValue::String(s) => toml::Value::String(s.clone()),
        JsonValue::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for (index, item) in items.iter().enumerate() {
                let child_path = format!("{path}[{index}]");
                out.push(json_to_toml(item, &child_path)?);
            }
            toml::Value::Array(out)
        }
        JsonValue::Object(map) => {
            let mut table = toml::map::Map::new();
            for (key, item) in map {
                let child_path = format!("{path}.{key}");
                table.insert(key.clone(), json_to_toml(item, &child_path)?);
            }
            toml::Value::Table(table)
        }
        JsonValue::Null => return Err(format!("{path}: 不支持 null")),
    })
}

fn toml_to_json(value: &toml::Value) -> Option<JsonValue> {
    Some(match value {
        toml::Value::String(s) => JsonValue::String(s.clone()),
        toml::Value::Integer(i) => JsonValue::from(*i),
        toml::Value::Float(f) => JsonValue::from(*f),
        toml::Value::Boolean(b) => JsonValue::Bool(*b),
        toml::Value::Datetime(d) => JsonValue::String(d.to_string()),
        toml::Value::Array(items) => {
            JsonValue::Array(items.iter().map(toml_to_json).collect::<Option<Vec<_>>>()?)
        }
        toml::Value::Table(map) => JsonValue::Object(
            map.iter()
                .map(|(k, v)| toml_to_json(v).map(|v| (k.clone(), v)))
                .collect::<Option<serde_json::Map<_, _>>>()?,
        ),
    })
}

// ---------- 字段构造辅助 ----------

fn base(config: &AppConfig, path: &str, label: &str, kind: FieldKind) -> SettingsField {
    let value = match get_value(config, path) {
        Some(value) if !value.is_null() => value,
        // 可选字段（Option / 空表）在 TOML 里不存在，给前端一个空值占位
        _ => JsonValue::String(String::new()),
    };
    SettingsField {
        path: path.to_string(),
        label: label.to_string(),
        help: String::new(),
        kind,
        value,
        options: Vec::new(),
        min: None,
        max: None,
        step: None,
        restart_required: is_restart_required(path),
        advanced: false,
    }
}

fn bool_(config: &AppConfig, path: &str, label: &str, help: &str) -> SettingsField {
    SettingsField {
        help: help.into(),
        ..base(config, path, label, FieldKind::Bool)
    }
}
fn bool_adv(config: &AppConfig, path: &str, label: &str, help: &str) -> SettingsField {
    SettingsField {
        advanced: true,
        ..bool_(config, path, label, help)
    }
}
fn int(
    config: &AppConfig,
    path: &str,
    label: &str,
    help: &str,
    min: f64,
    max: f64,
) -> SettingsField {
    SettingsField {
        help: help.into(),
        min: Some(min),
        max: Some(max),
        step: Some(1.0),
        ..base(config, path, label, FieldKind::Int)
    }
}
fn int_adv(
    config: &AppConfig,
    path: &str,
    label: &str,
    help: &str,
    min: f64,
    max: f64,
) -> SettingsField {
    SettingsField {
        advanced: true,
        ..int(config, path, label, help, min, max)
    }
}
fn float(
    config: &AppConfig,
    path: &str,
    label: &str,
    help: &str,
    min: f64,
    max: f64,
    step: f64,
) -> SettingsField {
    SettingsField {
        help: help.into(),
        min: Some(min),
        max: Some(max),
        step: Some(step),
        ..base(config, path, label, FieldKind::Float)
    }
}
fn float_adv(
    config: &AppConfig,
    path: &str,
    label: &str,
    help: &str,
    min: f64,
    max: f64,
) -> SettingsField {
    SettingsField {
        advanced: true,
        ..float(config, path, label, help, min, max, 0.01)
    }
}
fn string(config: &AppConfig, path: &str, label: &str, help: &str) -> SettingsField {
    SettingsField {
        help: help.into(),
        ..base(config, path, label, FieldKind::String)
    }
}
fn string_adv(config: &AppConfig, path: &str, label: &str, help: &str) -> SettingsField {
    SettingsField {
        advanced: true,
        ..string(config, path, label, help)
    }
}
fn secret(config: &AppConfig, path: &str, label: &str, help: &str) -> SettingsField {
    SettingsField {
        help: help.into(),
        ..base(config, path, label, FieldKind::Secret)
    }
}
fn secret_adv(config: &AppConfig, path: &str, label: &str, help: &str) -> SettingsField {
    SettingsField {
        advanced: true,
        ..secret(config, path, label, help)
    }
}
fn multiline(config: &AppConfig, path: &str, label: &str, help: &str) -> SettingsField {
    SettingsField {
        help: help.into(),
        ..base(config, path, label, FieldKind::Multiline)
    }
}
fn color(config: &AppConfig, path: &str, label: &str, help: &str) -> SettingsField {
    SettingsField {
        help: help.into(),
        ..base(config, path, label, FieldKind::Color)
    }
}
fn toml(config: &AppConfig, path: &str, label: &str, help: &str) -> SettingsField {
    SettingsField {
        help: help.into(),
        ..base(config, path, label, FieldKind::Toml)
    }
}
fn device(config: &AppConfig, path: &str, label: &str, help: &str) -> SettingsField {
    SettingsField {
        help: help.into(),
        ..base(config, path, label, FieldKind::Device)
    }
}
fn language(
    config: &AppConfig,
    path: &str,
    label: &str,
    help: &str,
    backend: &str,
) -> SettingsField {
    SettingsField {
        help: help.into(),
        options: crate::asr::languages::options_for_backend(backend)
            .into_iter()
            .map(|item| FieldOption {
                value: item.code,
                label: item.label,
            })
            .collect(),
        ..base(config, path, label, FieldKind::Language)
    }
}
fn enum_(
    config: &AppConfig,
    path: &str,
    label: &str,
    help: &str,
    options: Vec<(&str, &str)>,
) -> SettingsField {
    SettingsField {
        help: help.into(),
        options: options
            .into_iter()
            .map(|(value, label)| FieldOption {
                value: value.to_string(),
                label: label.to_string(),
            })
            .collect(),
        ..base(config, path, label, FieldKind::Enum)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn every_schema_path_maps_to_config() {
        let cfg = AppConfig::default();
        let schema = build(&cfg);
        let mut failures = Vec::new();
        for group in &schema.groups {
            for field in &group.fields {
                const OPTIONAL: &[&str] = &[
                    "vad.silero_model",
                    "asr.api_key",
                    "asr.auth_header",
                    "asr.edge.trusted_client_token",
                    "asr.edge.chromium_full_version",
                    "asr.edge.origin",
                    "translate.extra_body",
                ];
                if get_value(&cfg, &field.path).is_none()
                    && !OPTIONAL.contains(&field.path.as_str())
                {
                    failures.push(format!("{} 无法映射到 AppConfig", field.path));
                    continue;
                }
                let mut copy = cfg.clone();
                if let Err(e) = apply_change(&mut copy, &field.path, &field.value) {
                    failures.push(format!("{} 写入失败: {e}", field.path));
                }
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    /// schema 给出的 min/max 必须真能保存。
    /// 回归：vad.max_speech_ms / window_width / window_height 的滑块上限
    /// 超过 `validate()` 允许的范围，line_alphas 干脆没有范围，
    /// 于是 UI 能选出的值一保存就被后端拒。
    #[test]
    fn schema_ranges_pass_validation() {
        let base = AppConfig::default();
        let schema = build(&base);
        let mut failures = Vec::new();
        for group in &schema.groups {
            for field in &group.fields {
                let (Some(min), Some(max)) = (field.min, field.max) else {
                    continue;
                };
                if !matches!(field.kind, FieldKind::Int | FieldKind::Float) {
                    continue;
                }
                let is_array = field.path.ends_with("[]");
                for edge in [min, max] {
                    let value = match (field.kind, is_array) {
                        (FieldKind::Int, false) => json!(edge as i64),
                        (FieldKind::Int, true) => json!([edge as i64, edge as i64, edge as i64]),
                        (FieldKind::Float, false) => json!(edge),
                        (FieldKind::Float, true) => json!([edge, edge, edge]),
                        _ => continue,
                    };
                    let mut cfg = AppConfig::default();
                    if let Err(e) = apply_change(&mut cfg, &field.path, &value) {
                        failures.push(format!("{}={edge} 写入失败: {e}", field.path));
                        continue;
                    }
                    if let Err(e) = cfg.validate() {
                        failures.push(format!("{}={edge} 未通过 validate: {e}", field.path));
                    }
                }
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn apply_change_updates_values() {
        let mut cfg = AppConfig::default();
        assert!(!apply_change(&mut cfg, "subtitle.font_size", &JsonValue::from(40)).unwrap());
        assert_eq!(cfg.subtitle.font_size, 40);
        assert!(apply_change(&mut cfg, "translate.timeout_secs", &JsonValue::from(30)).unwrap());
        assert_eq!(cfg.translate.timeout_secs, 30);
        apply_change(
            &mut cfg,
            "subtitle.label_sources",
            &JsonValue::from("never"),
        )
        .unwrap();
        assert_eq!(cfg.subtitle.label_sources, crate::config::LabelMode::Never);
    }

    #[test]
    fn extra_body_accepts_toml_string() {
        let mut cfg = AppConfig::default();
        apply_change(
            &mut cfg,
            "translate.extra_body",
            &JsonValue::from("thinking = { type = \"disabled\" }"),
        )
        .unwrap();
        assert!(cfg.translate.extra_body.is_some());

        // 结构化 JSON 对象（如从 SHIPPED_DEFAULT 读取再回写）不能把子字符串误当作 TOML 文档解析
        let shipped = AppConfig::from_str(AppConfig::SHIPPED_DEFAULT).unwrap();
        let schema = build(&shipped);
        let field = schema
            .groups
            .iter()
            .flat_map(|g| &g.fields)
            .find(|f| f.path == "translate.extra_body")
            .unwrap();
        let mut copy = shipped.clone();
        apply_change(&mut copy, &field.path, &field.value).unwrap();
        assert_eq!(copy.translate.extra_body, shipped.translate.extra_body);
    }

    #[test]
    fn schema_has_all_major_groups() {
        let schema = build(&AppConfig::default());
        let ids: Vec<_> = schema.groups.iter().map(|g| g.id.as_str()).collect();
        for expected in [
            "sources",
            "vad",
            "asr",
            "translate",
            "subtitle",
            "steamvr",
            "osc",
            "advanced",
        ] {
            assert!(ids.contains(&expected), "缺少分组 {expected}");
        }
    }

    #[test]
    fn language_fields_carry_backend_specific_options_and_hot_reload() {
        let schema = build(&AppConfig::default());
        let asr_group = schema.groups.iter().find(|g| g.id == "asr").unwrap();
        let whisper_lang = asr_group
            .fields
            .iter()
            .find(|f| f.path == "asr.language")
            .unwrap();
        let edge_lang = asr_group
            .fields
            .iter()
            .find(|f| f.path == "asr.edge.language")
            .unwrap();

        assert!(whisper_lang.options.iter().any(|o| o.value == "auto"));
        assert!(!edge_lang.options.iter().any(|o| o.value == "auto"));
        assert!(edge_lang.options.iter().any(|o| o.value == "en-US"));
        assert!(!whisper_lang.restart_required);
        assert!(!edge_lang.restart_required);
    }
}

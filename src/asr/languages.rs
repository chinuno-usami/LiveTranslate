//! 识别语言候选
//!
//! 两个后端用的语言标识格式不同：
//! - Whisper 系列用 ISO-639-1 两位码（如 `zh`），并支持 `auto` 自动检测
//! - Edge 在线识别用 BCP-47（如 `zh-CN`），**不支持**自动检测

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct LanguageOption {
    pub code: String,
    pub label: String,
}

impl LanguageOption {
    fn new(code: &str, label: &str) -> Self {
        Self {
            code: code.to_string(),
            label: label.to_string(),
        }
    }
}

/// Whisper 兼容服务的语言候选（ISO-639-1）
pub const WHISPER_LANGUAGES: &[(&str, &str)] = &[
    ("auto", "自动检测"),
    ("zh", "中文"),
    ("en", "英语"),
    ("ja", "日语"),
    ("ko", "韩语"),
    ("fr", "法语"),
    ("de", "德语"),
    ("es", "西班牙语"),
    ("ru", "俄语"),
    ("pt", "葡萄牙语"),
    ("it", "意大利语"),
    ("ar", "阿拉伯语"),
    ("th", "泰语"),
    ("vi", "越南语"),
    ("id", "印尼语"),
    ("tr", "土耳其语"),
    ("nl", "荷兰语"),
    ("pl", "波兰语"),
    ("hi", "印地语"),
    ("uk", "乌克兰语"),
];

/// Edge 在线识别的语言候选（BCP-47，无自动检测）
pub const EDGE_LANGUAGES: &[(&str, &str)] = &[
    ("en-US", "英语（美国）"),
    ("en-GB", "英语（英国）"),
    ("zh-CN", "中文（简体）"),
    ("zh-TW", "中文（繁体）"),
    ("ja-JP", "日语"),
    ("ko-KR", "韩语"),
    ("fr-FR", "法语"),
    ("de-DE", "德语"),
    ("es-ES", "西班牙语"),
    ("ru-RU", "俄语"),
    ("pt-BR", "葡萄牙语（巴西）"),
    ("it-IT", "意大利语"),
    ("ar-SA", "阿拉伯语"),
    ("th-TH", "泰语"),
    ("vi-VN", "越南语"),
    ("id-ID", "印尼语"),
    ("tr-TR", "土耳其语"),
    ("nl-NL", "荷兰语"),
    ("pl-PL", "波兰语"),
    ("hi-IN", "印地语"),
];

/// 是否为 edge 后端
pub fn is_edge_backend(backend: &str) -> bool {
    backend.trim().eq_ignore_ascii_case("edge")
}

/// 按后端返回语言候选
pub fn options_for_backend(backend: &str) -> Vec<LanguageOption> {
    let table = if is_edge_backend(backend) {
        EDGE_LANGUAGES
    } else {
        WHISPER_LANGUAGES
    };

    table
        .iter()
        .map(|(code, label)| LanguageOption::new(code, label))
        .collect()
}

/// 把语言码归一化到当前后端可接受的形态
///
/// 主要用于切换后端后，旧配置里的语言码格式不匹配时给出合理回退：
/// - edge 不接受 `auto`，回退到首个候选
/// - whisper 不接受 BCP-47，取主语言子标签（`zh-CN` -> `zh`）
pub fn normalize_for_backend(backend: &str, language: &str) -> String {
    let language = language.trim();
    if language.is_empty() {
        return default_for_backend(backend);
    }

    if is_edge_backend(backend) {
        if language.eq_ignore_ascii_case("auto") {
            return default_for_backend(backend);
        }
        return language.to_string();
    }

    // whisper：取 "-" 之前的部分
    let primary = language.split('-').next().unwrap_or(language);
    if primary.is_empty() {
        default_for_backend(backend)
    } else {
        primary.to_ascii_lowercase()
    }
}

fn default_for_backend(backend: &str) -> String {
    if is_edge_backend(backend) {
        EDGE_LANGUAGES[0].0.to_string()
    } else {
        "auto".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whisper_options_include_auto_but_edge_does_not() {
        assert!(options_for_backend("whisper")
            .iter()
            .any(|o| o.code == "auto"));
        assert!(!options_for_backend("edge").iter().any(|o| o.code == "auto"));
    }

    #[test]
    fn backend_match_is_case_insensitive() {
        assert!(is_edge_backend("EDGE"));
        assert!(is_edge_backend(" Edge "));
        assert!(!is_edge_backend("whisper"));
    }

    #[test]
    fn edge_rejects_auto() {
        assert_eq!(normalize_for_backend("edge", "auto"), "en-US");
        assert_eq!(normalize_for_backend("edge", "zh-CN"), "zh-CN");
        assert_eq!(normalize_for_backend("edge", ""), "en-US");
    }

    #[test]
    fn whisper_takes_primary_subtag() {
        assert_eq!(normalize_for_backend("whisper", "zh-CN"), "zh");
        assert_eq!(normalize_for_backend("whisper", "en-US"), "en");
        assert_eq!(normalize_for_backend("whisper", "zh"), "zh");
        assert_eq!(normalize_for_backend("whisper", "auto"), "auto");
        assert_eq!(normalize_for_backend("whisper", ""), "auto");
    }
}

//! 识别结果清洗
//!
//! 能量型 VAD 分不清"音乐"和"人声"——两者都是持续的高能量音频。
//! 于是音乐片段也会被送去识别，而 Whisper 这类模型在音乐/噪声上
//! 会**稳定地产生幻听输出**（`[Music]`、`♪♪♪`、"字幕由…提供"、
//! "Thanks for watching" 等）。
//!
//! 这里做保守过滤：只丢掉明显不是语音内容的输出，避免误伤真实语句。

/// 通用标记词：归一化后必须**整段相等**才丢弃
///
/// 这些词在真实语句里也常见（"I love music"、"我订阅了"），不能做子串匹配。
const EXACT_HALLUCINATIONS: &[&str] = &[
    "music",
    "applause",
    "laughter",
    "silence",
    "blankaudio",
    "inaudible",
    "soundeffect",
    "noise",
    "音楽",
    "掌声",
    "笑声",
    "音乐",
    "静音",
    "訂閱",
    "订阅",
];

/// 特征明显的长短语：短文本中以子串出现即丢弃
const SUBSTRING_HALLUCINATIONS: &[&str] = &[
    "thanksforwatching",
    "thankyouforwatching",
    "pleasesubscribe",
    "subscribetomychannel",
    "subtitlesby",
    "subtitledby",
    "amaraorg",
    "字幕由",
    "字幕志愿者",
    "感谢观看",
    "谢谢观看",
    "请不吝点赞",
    "ご視聴ありがとうございました",
    "ご視聴ありがとう",
];

/// 超过这个长度就不做短语比对，避免误伤真实长句
const MAX_PHRASE_CHECK_LEN: usize = 40;

/// 整段被括号包裹（`[Music]` / `（掌声）`）时视为提示而非语音
fn is_wrapped_in_brackets(text: &str) -> bool {
    const PAIRS: &[(char, char)] = &[
        ('[', ']'),
        ('(', ')'),
        ('{', '}'),
        ('<', '>'),
        ('（', '）'),
        ('【', '】'),
        ('〔', '〕'),
        ('《', '》'),
    ];

    let Some(first) = text.chars().next() else {
        return false;
    };
    let Some(&(open, close)) = PAIRS.iter().find(|(o, _)| *o == first) else {
        return false;
    };

    // 首字符的配对闭括号必须恰好是最后一个字符：
    // "(laughs) Hello (sighs)" 首尾虽是括号，但中间有正文，不能丢弃
    let mut depth = 0usize;
    let count = text.chars().count();
    for (i, c) in text.chars().enumerate() {
        if c == open {
            depth += 1;
        } else if c == close {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                return i == count - 1;
            }
        }
    }
    false
}

/// 归一化：转小写并去掉所有空白与标点符号
fn normalize(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_whitespace() && !c.is_ascii_punctuation())
        .filter(|c| !matches!(c, '，' | '。' | '！' | '？' | '、' | '；' | '：' | '“' | '”' | '‘' | '’' | '（' | '）' | '【' | '】'))
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// 判断识别结果是否是可用语音内容
///
/// 返回 `false` 表示应丢弃（音乐/噪声引起的无意义输出）。
pub fn is_meaningful_speech(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return false;
    }

    // 去掉符号后没有任何字母/数字/汉字 -> 只是符号（♪ ♫ … 之类）
    if !trimmed.chars().any(|c| c.is_alphanumeric()) {
        return false;
    }

    // 整段被括号包裹 -> 提示性内容
    if is_wrapped_in_brackets(trimmed) {
        return false;
    }

    // 短文本才做幻听短语比对，长句不比对以免误伤
    if trimmed.chars().count() <= MAX_PHRASE_CHECK_LEN {
        let normalized = normalize(trimmed);
        if !normalized.is_empty()
            && (EXACT_HALLUCINATIONS.iter().any(|p| normalized == *p)
                || SUBSTRING_HALLUCINATIONS.iter().any(|p| normalized.contains(p)))
        {
            return false;
        }
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drops_empty_and_symbol_only() {
        assert!(!is_meaningful_speech(""));
        assert!(!is_meaningful_speech("   "));
        assert!(!is_meaningful_speech("♪♪♪"));
        assert!(!is_meaningful_speech("..."));
        assert!(!is_meaningful_speech("——"));
    }

    #[test]
    fn drops_bracketed_markers() {
        assert!(!is_meaningful_speech("[Music]"));
        assert!(!is_meaningful_speech("[BLANK_AUDIO]"));
        assert!(!is_meaningful_speech("（掌声）"));
        assert!(!is_meaningful_speech("【音乐】"));
        assert!(!is_meaningful_speech("[ Silence ]"));
    }

    #[test]
    fn drops_known_hallucinations() {
        assert!(!is_meaningful_speech("Thanks for watching!"));
        assert!(!is_meaningful_speech("Please subscribe"));
        assert!(!is_meaningful_speech("字幕由 Amara.org 社区提供"));
        assert!(!is_meaningful_speech("ご視聴ありがとうございました"));
    }

    #[test]
    fn keeps_real_speech() {
        assert!(is_meaningful_speech("Hello, everyone."));
        assert!(is_meaningful_speech("今天我们来聊一个很有意思的话题"));
        assert!(is_meaningful_speech("The quick brown fox jumps over the lazy dog."));
        // 括号出现在句子中间不应被丢弃
        assert!(is_meaningful_speech("他说（大概）明天会来"));
        // 通用标记词出现在真实语句中不应丢弃
        assert!(is_meaningful_speech("I love music"));
        assert!(is_meaningful_speech("Keep the noise down"));
        assert!(is_meaningful_speech("我订阅了"));
        assert!(!is_meaningful_speech("Music."));
        // 首尾是括号但中间有正文
        assert!(is_meaningful_speech("(laughs) Hello there (sighs)"));
        assert!(is_meaningful_speech("[Music] hi [Music]"));
    }

    #[test]
    fn long_text_is_not_phrase_filtered() {
        // 长句即使包含敏感短语也不过滤，避免误伤
        let long = "This is a very long sentence that happens to contain the words thanks for watching \
                    but it is clearly a real utterance from the speaker in this recording.";
        assert!(long.chars().count() > MAX_PHRASE_CHECK_LEN);
        assert!(is_meaningful_speech(long));
    }
}

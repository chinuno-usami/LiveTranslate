use std::collections::VecDeque;

/// 字幕处理器 - 处理去重、拼接、段落合并等逻辑
pub struct SubtitleProcessor {
    /// 最近识别的文本（用于去重）
    recent_texts: VecDeque<String>,
    /// 最近翻译的文本（用于去重）
    recent_translations: VecDeque<String>,
    /// 历史大小
    history_size: usize,
}

impl SubtitleProcessor {
    pub fn new() -> Self {
        Self {
            recent_texts: VecDeque::with_capacity(3),
            recent_translations: VecDeque::with_capacity(3),
            history_size: 3,
        }
    }

    /// 检查是否重复
    pub fn is_duplicate(&self, text: &str, translation: &str) -> bool {
        // 检查源文本是否与最近一条相同或类似
        if let Some(last_text) = self.recent_texts.back() {
            if Self::similarity(text, last_text) > 0.95 {
                return true;
            }
        }

        // 检查译文是否与最近一条相同或类似
        if let Some(last_trans) = self.recent_translations.back() {
            if Self::similarity(translation, last_trans) > 0.95 {
                return true;
            }
        }

        false
    }

    /// 尝试做前缀/后缀去重 - 返回去重后的文本和翻译
    /// 用于处理重叠分片的情况
    pub fn deduplicate_overlap(
        &self,
        text: &str,
        translation: &str,
    ) -> (String, String) {
        let mut deduplicated_text = text.to_string();
        let mut deduplicated_translation = translation.to_string();

        // 如果最后一条有记录，尝试移除前缀重叠
        if let Some(last_text) = self.recent_texts.back() {
            // 尝试从当前文本中移除与上一条文本重叠的部分
            if let Some(dedup) = Self::remove_prefix_overlap(last_text, text) {
                deduplicated_text = dedup;
                tracing::debug!("Removed text prefix overlap: {} -> {}", text, deduplicated_text);
            }
        }

        if let Some(last_trans) = self.recent_translations.back() {
            if let Some(dedup) = Self::remove_prefix_overlap(last_trans, translation) {
                deduplicated_translation = dedup;
                tracing::debug!(
                    "Removed translation prefix overlap: {} -> {}",
                    translation,
                    deduplicated_translation
                );
            }
        }

        (deduplicated_text, deduplicated_translation)
    }

    /// 从 current 中移除与 previous 末尾相同的前缀
    fn remove_prefix_overlap(previous: &str, current: &str) -> Option<String> {
        let prev_words: Vec<&str> = previous.split_whitespace().collect();
        let curr_words: Vec<&str> = current.split_whitespace().collect();

        if prev_words.is_empty() || curr_words.is_empty() {
            return None;
        }

        // 尝试匹配末尾 N 个单词
        for overlap_count in (1..=prev_words.len().min(curr_words.len())).rev() {
            let prev_end = &prev_words[prev_words.len() - overlap_count..];
            let curr_start = &curr_words[..overlap_count];

            if prev_end == curr_start {
                // 找到重叠，移除
                let remaining: Vec<&str> = curr_words[overlap_count..].to_vec();
                if remaining.is_empty() {
                    return None;
                }
                return Some(remaining.join(" "));
            }
        }

        None
    }

    /// 记录已处理的字幕
    pub fn record(&mut self, text: &str, translation: &str) {
        self.recent_texts.push_back(text.to_string());
        if self.recent_texts.len() > self.history_size {
            self.recent_texts.pop_front();
        }

        self.recent_translations.push_back(translation.to_string());
        if self.recent_translations.len() > self.history_size {
            self.recent_translations.pop_front();
        }
    }

    /// 计算两个字符串的相似度 (Jaro-Winkler 简化版)
    fn similarity(a: &str, b: &str) -> f32 {
        if a == b {
            return 1.0;
        }

        let a_lower = a.to_lowercase();
        let b_lower = b.to_lowercase();

        if a_lower == b_lower {
            return 0.99;
        }

        // 简单的编辑距离相似度
        let max_len = a.len().max(b.len());
        if max_len == 0 {
            return 1.0;
        }

        let distance = Self::levenshtein_distance(&a_lower, &b_lower);
        1.0 - (distance as f32 / max_len as f32)
    }

    /// 计算 Levenshtein 距离
    fn levenshtein_distance(a: &str, b: &str) -> usize {
        let a_chars: Vec<char> = a.chars().collect();
        let b_chars: Vec<char> = b.chars().collect();

        let mut matrix = vec![vec![0; b_chars.len() + 1]; a_chars.len() + 1];

        for i in 0..=a_chars.len() {
            matrix[i][0] = i;
        }
        for j in 0..=b_chars.len() {
            matrix[0][j] = j;
        }

        for i in 1..=a_chars.len() {
            for j in 1..=b_chars.len() {
                let cost = if a_chars[i - 1] == b_chars[j - 1] { 0 } else { 1 };
                matrix[i][j] = *[
                    matrix[i - 1][j] + 1,      // deletion
                    matrix[i][j - 1] + 1,      // insertion
                    matrix[i - 1][j - 1] + cost, // substitution
                ]
                .iter()
                .min()
                .unwrap();
            }
        }

        matrix[a_chars.len()][b_chars.len()]
    }
}

impl Default for SubtitleProcessor {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_remove_prefix_overlap() {
        let prev = "hello world how";
        let curr = "how are you";
        let result = SubtitleProcessor::remove_prefix_overlap(prev, curr);
        assert_eq!(result, Some("are you".to_string()));
    }

    #[test]
    fn test_no_overlap() {
        let prev = "hello world";
        let curr = "goodbye world";
        let result = SubtitleProcessor::remove_prefix_overlap(prev, curr);
        assert_eq!(result, None);
    }

    #[test]
    fn test_similarity() {
        assert!(SubtitleProcessor::similarity("hello", "hello") > 0.95);
        assert!(SubtitleProcessor::similarity("hello", "hallo") > 0.7);
        assert!(SubtitleProcessor::similarity("abc", "xyz") < 0.5);
    }
}

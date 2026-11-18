//! 推理控制标签清洗。
//!
//! 部分 Chat 兼容上游会把 `<thought>` / `<think>` 的闭合标签写进
//! 可见 `content`。这些标签不是业务文本，直接透传会污染客户端历史；一旦模型
//! 在下一轮看到自己的闭合标签，还会继续补更多标签，形成自强化退化。
//!
//! 这里只处理明确的控制标签，不碰普通 XML/HTML：
//! - `<thought>` 及其闭合标签
//! - `<think>` 及其闭合标签
//!
//! 流式路径必须保留跨分片的半个标签，因此 `ReasoningTagSanitizer` 会暂存
//! 末尾可能是标签前缀的部分，等下一片到达后再判断。

// 闭合 think 标签用 concat! 拆开书写，避免被某些传输/编辑器层当成控制标签吞掉。
const TAGS: [&str; 4] = [
    "<thought>",
    "</thought>",
    "<think>",
    concat!("</", "think>"),
];

/// 最长的控制标签长度；流式暂存只可能发生在这么长的后缀内。
const MAX_TAG_LEN: usize = 10;

/// 清洗一段完整文本，返回清洗后的文本与被移除的标签数。
pub(crate) fn sanitize_reasoning_tags(input: &str) -> (String, usize) {
    let mut output = String::with_capacity(input.len());
    let mut removed = 0usize;
    let mut index = 0usize;

    while index < input.len() {
        if input.as_bytes()[index] == b'<' {
            if let Some(length) = match_tag(&input[index..]) {
                removed += 1;
                index += length;
                continue;
            }
        }
        let Some(character) = input[index..].chars().next() else {
            break;
        };
        output.push(character);
        index += character.len_utf8();
    }

    (output, removed)
}

/// 返回从 `text` 开头开始匹配到的控制标签长度。
fn match_tag(text: &str) -> Option<usize> {
    TAGS.iter().find_map(|tag| {
        if tag.is_empty() {
            return None;
        }
        text.get(..tag.len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(tag))
            .then_some(tag.len())
    })
}

/// 找到末尾可能尚未完成的控制标签起点。
///
/// 例如 `abc</thou` 返回 `</thou` 的位置；`abc < def` 返回 `None`。
fn partial_tag_start(text: &str) -> Option<usize> {
    let start = text.rfind('<')?;
    let suffix = &text[start..];
    if suffix.len() > MAX_TAG_LEN {
        return None;
    }
    let lower = suffix.to_ascii_lowercase();
    TAGS.iter()
        .any(|tag| lower.len() < tag.len() && tag.starts_with(&lower))
        .then_some(start)
}

/// 流式清洗器：跨分片保留半个控制标签。
#[derive(Default)]
pub(crate) struct ReasoningTagSanitizer {
    pending: String,
    removed: usize,
}

impl ReasoningTagSanitizer {
    pub(crate) fn push(&mut self, chunk: &str) -> String {
        if chunk.is_empty() {
            return String::new();
        }
        self.pending.push_str(chunk);

        let safe = match partial_tag_start(&self.pending) {
            Some(position) => {
                let tail = self.pending.split_off(position);
                std::mem::replace(&mut self.pending, tail)
            }
            None => std::mem::take(&mut self.pending),
        };
        let (text, removed) = sanitize_reasoning_tags(&safe);
        self.removed += removed;
        text
    }

    /// 流结束。未完成的半个标签原样返回，宁可显示一个截断标签，也不丢用户文本。
    pub(crate) fn finish(&mut self) -> String {
        let pending = std::mem::take(&mut self.pending);
        let (text, removed) = sanitize_reasoning_tags(&pending);
        self.removed += removed;
        text
    }

    pub(crate) fn removed(&self) -> usize {
        self.removed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_complete_reasoning_tags() {
        let (text, removed) = sanitize_reasoning_tags("回答</thought>继续");
        assert_eq!(text, "回答继续");
        assert_eq!(removed, 1);
    }

    #[test]
    fn preserves_unicode_text_when_less_than_prefix_is_not_a_tag() {
        let input = "<1234567中后续中文";
        let (text, removed) = sanitize_reasoning_tags(input);
        assert_eq!(text, input);
        assert_eq!(removed, 0);
    }

    #[test]
    fn strips_repeated_closing_tags() {
        let input = format!("完成{}", "</thought>".repeat(64));
        let (text, removed) = sanitize_reasoning_tags(&input);
        assert_eq!(text, "完成");
        assert_eq!(removed, 64);
    }

    #[test]
    fn handles_codex_degeneration_scale_without_leaking() {
        let input = format!("完成{}", "</thought>".repeat(8_000));
        let (text, removed) = sanitize_reasoning_tags(&input);
        assert_eq!(text, "完成");
        assert_eq!(removed, 8_000);
    }

    #[test]
    fn does_not_strip_unrelated_xml() {
        let input = "<thoughtful>ok</thoughtful>";
        let (text, removed) = sanitize_reasoning_tags(input);
        assert_eq!(text, input);
        assert_eq!(removed, 0);
    }

    #[test]
    fn stream_holds_split_closing_tag() {
        let mut sanitizer = ReasoningTagSanitizer::default();
        assert_eq!(sanitizer.push("回答</thou"), "回答");
        assert_eq!(sanitizer.push("ght>继续"), "继续");
        assert_eq!(sanitizer.finish(), "");
        assert_eq!(sanitizer.removed(), 1);
    }

    #[test]
    fn stream_keeps_plain_less_than() {
        let mut sanitizer = ReasoningTagSanitizer::default();
        assert_eq!(sanitizer.push("a < b"), "a < b");
        assert_eq!(sanitizer.finish(), "");
        assert_eq!(sanitizer.removed(), 0);
    }

    #[test]
    fn stream_flushes_partial_tag_at_end() {
        let mut sanitizer = ReasoningTagSanitizer::default();
        assert_eq!(sanitizer.push("回答</thou"), "回答");
        assert_eq!(sanitizer.finish(), "</thou");
        assert_eq!(sanitizer.removed(), 0);
    }
}

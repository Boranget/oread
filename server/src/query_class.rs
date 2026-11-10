//! 查询分类：把「这个源对哪类查询有产出」分开记，避免热门书与冷门书互相污染。
//!
//! 为什么需要：体检语料只有 24 条热门书。`百年孤独` 和 `斗破苍穹` 在「哪些源会命中」
//! 上是两个不同的分布；如果只用一个全局产出率，冷门查询会被热门查询的证据掩盖。
//!
//! 分类只影响**入队顺序**，不改变参与集合与返回结果；分错类的代价是顺序略差，
//! 不会让本来能搜到的书消失。因此这里用便宜、可解释的启发式，并始终有 `normal` 兜底。
//!
//! 设计见 `docs/DESIGN_SEARCH.md` 第 4 节。

use crate::search_rank::normalize_title;
use crate::source_bench::BUILTIN_CORPUS;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum QueryClass {
    /// 命中体检语料的热门书（先验最可信）
    Hot,
    /// 普通查询（兜底）
    #[default]
    Normal,
    /// 很短或很长的查询：大概率是冷门/长尾
    Obscure,
    /// 显式按作者搜索
    Author,
    /// 带章节/卷号
    Serial,
}

impl QueryClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hot => "hot",
            Self::Normal => "normal",
            Self::Obscure => "obscure",
            Self::Author => "author",
            Self::Serial => "serial",
        }
    }

    /// 未知/空值一律回落 `Normal`：分类是启发式，不能因为脏数据丢掉证据。
    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "hot" => Self::Hot,
            "obscure" => Self::Obscure,
            "author" => Self::Author,
            "serial" => Self::Serial,
            _ => Self::Normal,
        }
    }

    /// 按关键词归类。顺序：作者 > 章节号 > 语料热门 > 长度冷门 > 普通。
    pub fn classify(raw: &str) -> Self {
        let s = raw.trim();
        if s.is_empty() {
            return Self::Normal;
        }
        if is_author_query(s) {
            return Self::Author;
        }
        if has_serial_marker(s) {
            return Self::Serial;
        }
        if is_corpus_title(s) {
            return Self::Hot;
        }
        let n = normalize_title(s).chars().count();
        if n <= 2 || n >= 12 {
            return Self::Obscure;
        }
        Self::Normal
    }
}

fn is_author_query(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    s.starts_with("作者:")
        || s.starts_with("作者：")
        || lower.starts_with("author:")
        || lower.starts_with("author：")
        || lower.starts_with("by ")
}

fn has_serial_marker(s: &str) -> bool {
    if s.contains('第')
        && s.chars()
            .any(|c| matches!(c, '章' | '节' | '卷' | '回' | '集'))
    {
        return true;
    }
    // 结尾 3 位以上数字：多为「书名 1234」这类续章/分卷查询
    s.trim_end()
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_digit())
        .count()
        >= 3
}

/// 是否命中内置体检语料的书名（归一化后互相包含）。
///
/// 只用于「热门」先验，不参与过滤；`scored=false` 的语料（出版书）同样算热门，
/// 因为它们的价值是「站点覆盖常见书名」，与是否计分无关。
pub fn is_corpus_title(raw: &str) -> bool {
    let q = normalize_title(raw);
    if q.is_empty() {
        return false;
    }
    let qn = q.chars().count();
    BUILTIN_CORPUS.iter().any(|c| {
        let t = normalize_title(c.title);
        if t.is_empty() {
            return false;
        }
        if t == q {
            return true;
        }
        // 只允许「查询包含完整语料书名」或「长度≥2 的查询被语料书名包含」，
        // 避免单字查询把整类都判成热门。
        (qn >= 2 && t.contains(&q)) || q.contains(&t)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_corpus_and_author_and_serial() {
        assert_eq!(QueryClass::classify("斗破苍穹"), QueryClass::Hot);
        assert_eq!(QueryClass::classify("《三体》"), QueryClass::Hot);
        assert_eq!(QueryClass::classify("作者：余华"), QueryClass::Author);
        assert_eq!(QueryClass::classify("by 天蚕土豆"), QueryClass::Author);
        assert_eq!(QueryClass::classify("斗破苍穹 第12章"), QueryClass::Serial);
        assert_eq!(QueryClass::classify("凡人修仙传1234"), QueryClass::Serial);
    }

    #[test]
    fn falls_back_to_normal_and_obscure() {
        assert_eq!(QueryClass::classify("百年孤独"), QueryClass::Normal);
        assert_eq!(QueryClass::classify("剑"), QueryClass::Obscure);
        assert_eq!(
            QueryClass::classify("一个特别特别长的冷门书名测试用例"),
            QueryClass::Obscure
        );
        assert_eq!(QueryClass::classify(""), QueryClass::Normal);
    }

    #[test]
    fn roundtrips_and_unknown_is_normal() {
        for c in [
            QueryClass::Hot,
            QueryClass::Normal,
            QueryClass::Obscure,
            QueryClass::Author,
            QueryClass::Serial,
        ] {
            assert_eq!(QueryClass::parse(c.as_str()), c);
        }
        assert_eq!(QueryClass::parse("nonsense"), QueryClass::Normal);
        assert_eq!(QueryClass::parse("  "), QueryClass::Normal);
    }
}

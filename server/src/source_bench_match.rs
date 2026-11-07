//! source_bench 的书名/作者归一化与匹配（move-only 从 source_bench.rs 拆出）。

pub(super) fn fullwidth_to_half(ch: char) -> char {
    let code = ch as u32;
    if (0xFF01..=0xFF5E).contains(&code) {
        char::from_u32(code - 0xFEE0).unwrap_or(ch)
    } else if code == 0x3000 {
        ' '
    } else {
        ch
    }
}

pub(super) fn is_noise_punct(c: char) -> bool {
    matches!(
        c,
        '《' | '》'
            | '「'
            | '」'
            | '『'
            | '』'
            | '（'
            | '）'
            | '('
            | ')'
            | '：'
            | ':'
            | '·'
            | '・'
            | '、'
            | '，'
            | ','
            | '。'
            | '.'
            | '！'
            | '!'
            | '？'
            | '?'
            | '—'
            | '-'
            | '_'
            | '~'
            | '～'
            | '"'
            | '\''
            | '“'
            | '”'
            | '‘'
            | '’'
            | '/'
            | '|'
            | '+'
            | '&'
            | '['
            | ']'
            | '【'
            | '】'
            | '<'
            | '>'
            | '*'
            | '#'
            | '@'
            | '%'
            | '×'
            | '✕'
    )
}

/// 标题归一化：去空白、全半角统一、去常见标点；**不做繁简转换**。
pub fn normalize_title(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        let c = fullwidth_to_half(ch);
        if c.is_whitespace() || is_noise_punct(c) {
            continue;
        }
        for lc in c.to_lowercase() {
            out.push(lc);
        }
    }
    out
}

pub(super) const TITLE_DECORATIONS: &[&str] = &[
    "最新章节",
    "全文阅读",
    "免费阅读",
    "在线阅读",
    "无弹窗",
    "全文",
    "小说",
];

pub(super) fn strip_decorations(mut s: String) -> String {
    loop {
        let mut changed = false;
        for suffix in TITLE_DECORATIONS {
            if s.len() > suffix.len() && s.ends_with(suffix) {
                let keep = s.len() - suffix.len();
                s.truncate(keep);
                changed = true;
            }
        }
        if !changed {
            return s;
        }
    }
}

pub(super) fn norm_len(s: &str) -> usize {
    s.chars().count()
}

pub(super) fn norm_pair(got: &str, expected: &str) -> Option<(String, String)> {
    let a = strip_decorations(normalize_title(got));
    let b = strip_decorations(normalize_title(expected));
    if a.is_empty() || b.is_empty() {
        None
    } else {
        Some((a, b))
    }
}

/// 衍生作/外传的常见标记：长标题比短标题多出来的部分含这些词时，不算“近似同名”。
/// 目的：`斗破苍穹外传` / `斗破苍穹之少年归来` 这类同人衍生不能靠标题包含混进命中。
pub(super) const DERIVATIVE_HINTS: &[&str] = &[
    "外传", "番外", "前传", "后传", "续集", "续作", "同人", "改编", "打卡", "之", "从",
];

pub(super) fn has_derivative_extra(longer: &str, base: &str) -> bool {
    let extra = longer.replacen(base, "", 1);
    DERIVATIVE_HINTS.iter().any(|h| extra.contains(h))
}

/// (a) 归一化标题近似相等：长度比 ≥ 0.6 且包含关系成立，且多出来的部分不含衍生作标记。
pub fn title_near_equal(got: &str, expected: &str) -> bool {
    let Some((a, b)) = norm_pair(got, expected) else {
        return false;
    };
    if a == b {
        return true;
    }
    if !(a.contains(&b) || b.contains(&a)) {
        return false;
    }
    let short = norm_len(&a).min(norm_len(&b));
    let long = norm_len(&a).max(norm_len(&b));
    if (short as f64 / long as f64) < 0.6 {
        return false;
    }
    let (longer, base) = if a.len() >= b.len() {
        (&a, &b)
    } else {
        (&b, &a)
    };
    !has_derivative_extra(longer, base)
}

/// 标题包含（不含长度/衍生限制），只用于诊断字段，不单独构成命中。
pub fn title_contains(got: &str, expected: &str) -> bool {
    let Some((a, b)) = norm_pair(got, expected) else {
        return false;
    };
    a.contains(&b) || b.contains(&a)
}

/// 诊断用：标题是否“相关”（近似同名或包含）。
pub fn title_matches(got: &str, expected: &str) -> bool {
    title_near_equal(got, expected) || title_contains(got, expected)
}

/// 命中判定：(a) 近似同名（作者不参与）或 (b) 标题包含 + 作者归一化后完全一致。
pub fn is_hit(
    got_title: &str,
    expected_title: &str,
    got_author: Option<&str>,
    expected_author: Option<&str>,
) -> bool {
    if title_near_equal(got_title, expected_title) {
        return true;
    }
    title_contains(got_title, expected_title) && author_matches(got_author, expected_author)
}

/// 作者归一化：在标题归一化的基础上去掉“作者：”前缀与常见署名后缀。
pub fn normalize_author(raw: &str) -> String {
    let mut s = normalize_title(raw);
    // 站点常见写法“作者：天蚕土豆”，归一化后是“作者天蚕土豆”
    while s.chars().count() > 2 && s.starts_with("作者") {
        s = s.chars().skip(2).collect();
    }
    for suffix in ["著", "编", "译", "繪", "绘", "作者"] {
        while s.ends_with(suffix) && norm_len(&s) > norm_len(suffix) {
            let keep = s.len() - suffix.len();
            s.truncate(keep);
        }
    }
    s
}

/// 作者匹配：expected 为空视为不校验；否则任何一位作者归一化后相等即算匹配。
pub fn author_matches(got: Option<&str>, expected: Option<&str>) -> bool {
    let Some(exp) = expected.map(str::trim).filter(|s| !s.is_empty()) else {
        return true;
    };
    let Some(got) = got.map(str::trim).filter(|s| !s.is_empty()) else {
        return false;
    };
    let want = normalize_author(exp);
    if want.is_empty() {
        return true;
    }
    got.split(['/', '、', ',', '，', ';', '；', '&'])
        .map(normalize_author)
        .any(|a| a == want)
}

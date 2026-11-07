//! 替换规则路由的替换规则执行（move-only 从 replace.rs 拆出）。

use super::*;

// ---------------- 替换规则在正文输出时生效 ----------------

/// 读取用户是否开启替换规则（缺省开启）
pub async fn replace_rule_enabled(db: &SqlitePool) -> bool {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT value FROM user_settings WHERE key='use_replace_rule'")
            .fetch_optional(db)
            .await
            .ok()
            .flatten();
    row.map(|(v,)| v != "0" && v != "false").unwrap_or(true)
}

/// 读取用户设置的正文缓存上限（MB）；未设置/非法值返回 None（表示沿用全局配置）。
pub async fn user_cache_limit_mb(db: &SqlitePool) -> Option<i64> {
    let v: Option<String> =
        sqlx::query_scalar("SELECT value FROM user_settings WHERE key='cache_limit_mb'")
            .fetch_optional(db)
            .await
            .ok()
            .flatten();
    v.and_then(|x| x.trim().parse::<i64>().ok())
        .filter(|m| *m > 0)
}

/// 读取用户图片样式：0 原始 / 1 宽度自适应 / 2 居中 / 3 隐藏。
pub async fn user_image_style(db: &SqlitePool) -> i64 {
    let v: Option<String> =
        sqlx::query_scalar("SELECT value FROM user_settings WHERE key='image_style'")
            .fetch_optional(db)
            .await
            .ok()
            .flatten();
    v.and_then(|x| x.trim().parse::<i64>().ok()).unwrap_or(0)
}

/// 按图片样式处理正文 HTML（轻量正则改写，不引入 HTML 解析器）。
pub fn apply_image_style(input: &str, style: i64) -> String {
    use once_cell::sync::OnceCell;
    static IMG_RE: OnceCell<regex::Regex> = OnceCell::new();
    let re = IMG_RE.get_or_init(|| regex::Regex::new(r"(?is)<img\b[^>]*>").unwrap());
    match style {
        3 => re.replace_all(input, "").to_string(),
        1 => re
            .replace_all(input, |c: &regex::Captures| {
                merge_img_style(&c[0], "max-width:100%;height:auto;")
            })
            .to_string(),
        2 => re
            .replace_all(input, |c: &regex::Captures| {
                merge_img_style(&c[0], "display:block;margin:0 auto;")
            })
            .to_string(),
        _ => input.to_string(),
    }
}

pub(crate) fn merge_img_style(tag: &str, add: &str) -> String {
    // 已有 style 属性则追加，否则插入一个新的 style 属性
    if let Some(pos) = tag.to_lowercase().find("style=\"") {
        let at = pos + "style=\"".len();
        let mut out = String::with_capacity(tag.len() + add.len());
        out.push_str(&tag[..at]);
        out.push_str(add);
        out.push_str(&tag[at..]);
        return out;
    }
    if let Some(end) = tag.rfind('>') {
        let mut out = String::with_capacity(tag.len() + add.len() + 8);
        out.push_str(&tag[..end]);
        out.push_str(&format!(" style=\"{add}\""));
        out.push_str(&tag[end..]);
        return out;
    }
    tag.to_string()
}

/// Legado 作用域匹配：`scope` 每行一个书名或书源域名，命中任一即生效；
/// `excludeScope` 命中则跳过。空 scope 表示全体书籍。
pub(crate) fn scope_hit(scope: &Option<String>, book: &str, origin: &str) -> bool {
    let raw = match scope.as_deref() {
        Some(s) => s.trim(),
        None => return false,
    };
    if raw.is_empty() {
        return false;
    }
    raw.split(['\n', ';', '|'])
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .any(|s| book.contains(s) || origin.contains(s))
}

/// 按 `order` 顺序执行已启用规则。`for_title` 决定是否应用 scope_title / scope_content。
pub fn apply_rules(
    rules: &[ReplaceRule],
    book: &str,
    origin: &str,
    input: &str,
    for_title: bool,
) -> String {
    let mut out = input.to_string();
    for r in rules {
        if !r.is_enabled {
            continue;
        }
        if for_title && !r.scope_title || !for_title && !r.scope_content {
            continue;
        }
        if r.pattern.trim().is_empty() {
            continue;
        }
        if let Some(scope) = r.scope.as_deref() {
            if !scope.trim().is_empty() && !scope_hit(&r.scope, book, origin) {
                continue;
            }
        }
        if scope_hit(&r.exclude_scope, book, origin) {
            continue;
        }
        if r.is_regex {
            // 正则回溯风险：限制模式长度并要求可编译；编译失败跳过该条规则
            if r.pattern.len() > 512 {
                continue;
            }
            if let Ok(re) = regex::Regex::new(&r.pattern) {
                out = re.replace_all(&out, r.replacement.as_str()).into_owned();
            }
        } else {
            out = out.replace(&r.pattern, &r.replacement);
        }
    }
    out
}

/// 内置净化规则集（默认关闭；`builtin_clean=1` 时与用户规则走同一条 apply_rules 链路）。
///
/// 原则：**只按整行处理，且图案必须落在行首/行尾**，宁可漏删也不误删正文。
/// “武动乾坤”这类插入句中广告保留（实测错层小说网出现在“月如银盘，漫天繁星…”句中，
/// 若按全文匹配会连正文一起删掉）。
pub(crate) fn builtin_rules() -> Vec<ReplaceRule> {
    let mk = |name: &str, pattern: &str| ReplaceRule {
        id: None,
        name: Some(name.to_string()),
        group: Some("内置净化".to_string()),
        pattern: pattern.to_string(),
        replacement: String::new(),
        scope: None,
        scope_title: false,
        scope_content: true,
        exclude_scope: None,
        is_enabled: true,
        is_regex: true,
        timeout_millisecond: 1000,
        order: 0,
    };
    vec![
        // 一秒记住【xxx】www.xxx.com，更新快，无弹窗！
        mk(
            "去“一秒记住/本站首发”提示",
            r"(?m)^[^\n]*(?:一秒记住|记住本站|请记住本书首发域名|本书首发来自)[^\n]*$",
        ),
        // 本章未完请点击下一页 / 请点击下一页继续阅读
        mk(
            "去“下一页/本章未完”引导",
            r"(?m)^[^\n]*(?:本章未完|点击下一页|下一页继续阅读|请点击下一页|翻页继续阅读)[^\n]*$",
        ),
        // 独立成行的网址/域名广告（含 www 或裸域名）
        mk(
            "去网址/域名广告行",
            r"(?m)^[^\n]*(?:https?://|www\.[A-Za-z0-9.-]+\.[A-Za-z]{2,10}|(?:[A-Za-z0-9-]+\.)+(?:com|net|org|cc|info|la|xyz|top|cn|tw|me|io|vip))(?:/\S*)?[^\n]*$",
        ),
        // 源站反爬提示（只删该句，不按整行匹配，避免连正文一起吞掉）：
        // “阅读提示：为防止内容获取不全，请勿使用浏览器阅读模式。”及其变体
        mk(
            "去源站反爬阅读提示句",
            r"(?:阅读提示[：:][ \t]*[^\n。！!？?]{0,40}?|为防止内容(?:获取)?不全[，,、][ \t]*)(?:请勿|切勿|不要)(?:再|继续)?使用(?:浏览器)?(?:的)?(?:阅读模式|浏览模式)[，,。！!．]?",
        ),
        // 求票/举报/收藏等短推广句（整行不超过 80 字）
        mk(
            "去推广/求票短句",
            r"(?m)^[^\n]{0,40}(?:天才一秒记住|手机用户请浏览|章节错误[，,]?\s*点此举报|求推荐票|求月票|加入书签|投推荐票|无弹窗|无广告|请收藏本站)[^\n]{0,40}$",
        ),
        // 独立成行的书名推广（仅当整行只有书名+标点）
        mk(
            "去独立书名推广行",
            r"(?m)^\s*(?:<[^>]+>|\s)*[（(【\[]?(?:武动乾坤|斗破苍穹)[）)】\]]?[。！!．.]?(?:<[^>]+>|\s)*$",
        ),
    ]
}

/// 内置净化开关（缺省关闭）
pub(crate) async fn builtin_clean_enabled(db: &SqlitePool) -> bool {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT value FROM user_settings WHERE key='builtin_clean'")
            .fetch_optional(db)
            .await
            .ok()
            .flatten();
    row.map(|(v,)| v == "1" || v == "true").unwrap_or(false)
}

/// 正文输出前的统一净化入口：读取用户规则并应用到标题/正文。
pub async fn purify(
    db: &SqlitePool,
    book: &str,
    origin: &str,
    title: Option<String>,
    content: Option<String>,
) -> (Option<String>, Option<String>) {
    if !replace_rule_enabled(db).await {
        return (title, content);
    }
    let rules = active_rules(db).await;
    if rules.is_empty() {
        return (title, content);
    }
    let t = title.map(|t| apply_rules(&rules, book, origin, &t, true));
    let c = content.map(|c| apply_rules(&rules, book, origin, &c, false));
    (t, c)
}

/// 当前生效的全部替换规则（内置净化开关打开时排在最前，用户规则随后）。
/// 目录与正文共用同一份加载逻辑，避免两处行为分叉；调用方自行决定是否启用。
pub async fn active_rules(db: &SqlitePool) -> Vec<ReplaceRule> {
    let mut rules: Vec<ReplaceRule> = match sqlx::query_as(
        "SELECT * FROM replace_rules WHERE is_enabled=1 ORDER BY \"order\", id",
    )
    .fetch_all(db)
    .await
    {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    if builtin_clean_enabled(db).await {
        let mut all = builtin_rules();
        all.append(&mut rules);
        rules = all;
    }
    rules
}

/// 目录标题净化：规则只加载一次，按 `scope_title` 逐章应用。
/// 只影响返回给用户的标题，`book_chapters` 缓存保持原文，开关可随时切换。
pub async fn purify_titles(db: &SqlitePool, book: &str, origin: &str, titles: &mut [String]) {
    let rules = active_rules(db).await;
    if rules.is_empty() {
        return;
    }
    for t in titles.iter_mut() {
        *t = apply_rules(&rules, book, origin, t, true);
    }
}

//! Legado 规则引擎（AnalyzeRule 等价实现）
//!
//! 支持：CSS 选择器(scraper) / JsonPath / XPath(sxd-xpath, XML 源) / 正则 / 变量 `${name}` 展开 /
//! 整串 JS 段(`<js>`、`@js:`)切分 / 列表解析(get_string_list)。
//! 元素级 `&&`/`||`/`%%`/`@` 组合与索引/范围语法、WebView 渲染降级为后续增强项。

use std::fmt;

// JS 执行层；显式 path 保持 rule_engine.rs 作为入口文件。
#[path = "rule_engine_js.rs"]
mod js;
pub use js::{init_js, init_rule_cache_dir, js_metrics};

#[derive(Debug)]
pub enum RuleError {
    // 历史上 CSS 选择器解析失败用此变体；当前统一走 `Other`，变体保留以稳定错误分类。
    #[allow(dead_code)]
    Selector(String),
    Json(String),
    Regex(String),
    Other(String),
}

impl fmt::Display for RuleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RuleError::Selector(s) => write!(f, "selector: {s}"),
            RuleError::Json(s) => write!(f, "json: {s}"),
            RuleError::Regex(s) => write!(f, "regex: {s}"),
            RuleError::Other(s) => write!(f, "{s}"),
        }
    }
}

impl std::error::Error for RuleError {}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Css,
    XPath,
    Json,
    Js,
    Regex,
    Default,
}

#[path = "rule_engine_core.rs"]
mod core;
pub use core::{AnalyzeRule, BookMeta};

// 逻辑上属于 rule_engine 的子模块；用显式 path 保持 `rule_engine.rs` 单文件入口，
// 避免一次性把 8k 行源文件改成目录带来无关 diff。
#[path = "rule_engine_json_path.rs"]
mod json_path;

// HTML/CSS 选择器与取词辅助；显式 path 保持 rule_engine.rs 作为入口文件。
#[path = "rule_engine_html.rs"]
mod html;

#[cfg(test)]
#[path = "rule_engine_tests.rs"]
mod tests;

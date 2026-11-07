//! HTML/CSS 选择器与上游规则取词辅助（move-only 从 rule_engine.rs 拆出）。
//!
//! 这些函数同时服务 AnalyzeRule 的 CSS/CSS 增强语法与 JS 的 org.jsoup 桥；
//! 对外只暴露给父模块（pub(in crate::rule_engine)），不扩展 crate 公共 API。

use std::sync::OnceLock;

use regex::Regex;
use serde_json::Value;

#[path = "rule_engine_html_text.rs"]
mod text;
pub(in crate::rule_engine) use text::*;
#[path = "rule_engine_html_dom.rs"]
mod dom;
pub(in crate::rule_engine) use dom::*;
#[path = "rule_engine_html_css.rs"]
mod css;
pub(in crate::rule_engine) use css::*;
#[path = "rule_engine_html_jq.rs"]
mod jq;
pub(in crate::rule_engine) use jq::*;
#[path = "rule_engine_html_js.rs"]
mod js;
pub(in crate::rule_engine) use js::*;
#[path = "rule_engine_html_json.rs"]
mod json;
pub(in crate::rule_engine) use json::*;
#[path = "rule_engine_html_rule.rs"]
mod rule;
pub(in crate::rule_engine) use rule::*;

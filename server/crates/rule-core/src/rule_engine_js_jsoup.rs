//! org.jsoup JS-Java 桥（move-only 从 rule_engine_js.rs 拆出）。
//!
//! 用 scraper(html5ever) 解析 HTML，按节点预序编号做句柄；每次 JS 求值前清空注册表。

use std::cell::RefCell;

use scraper::{ElementRef, Html, Selector};

use super::super::html::{apply_jq_filters, split_jquery_filters, JqFilter};
use super::ajax_error;

/// 垫片抛出的“未支持 API”统一前缀；worker 把它翻译成用户能读懂的提示。
pub(super) const UNSUPPORTED_JS_API: &str = "UNSUPPORTED_JS_API:";

// ============================ org.jsoup JS-Java 桥 ============================
//
// 书源里的 `org.jsoup.Jsoup.parse(...)` 在 App 里是 Rhino 直接调用 jsoup 的 Java API。
// 本服务用 scraper(html5ever) 做 HTML5 解析与 CSS 匹配，按“节点预序编号”做句柄：
// 每次 eval 前清空注册表，句柄只在同一次 JS 求值内有效（与 App 的对象生命周期一致）。

pub(super) const JSOUP_MAX_DOCS: usize = 8;

pub(super) struct JsoupDoc {
    html: Html,
    base: String,
}

#[derive(Default)]
pub(super) struct JsoupRegistry {
    docs: Vec<JsoupDoc>,
    /// (doc 下标, 节点在文档中的先序序号)；序号 0 表示文档根（Document）。
    handles: Vec<(usize, usize)>,
}

thread_local! {
    static JSOUP_REG: RefCell<JsoupRegistry> = RefCell::new(JsoupRegistry::default());
}

/// 每次 JS 任务开始前清空，避免句柄/文档跨任务泄漏。
pub(crate) fn jsoup_clear_registry() {
    JSOUP_REG.with(|r| {
        let mut r = r.borrow_mut();
        r.docs.clear();
        r.handles.clear();
    });
}

pub(super) fn jsoup_arg_str(args: &[boa_engine::JsValue], i: usize) -> String {
    args.get(i)
        .and_then(|v| v.as_string())
        .map(|s| s.to_std_string_escaped())
        .unwrap_or_default()
}

pub(super) fn jsoup_arg_handle(args: &[boa_engine::JsValue], i: usize) -> Option<u32> {
    let n = args.get(i)?.as_number()?;
    if !n.is_finite() || n <= 0.0 {
        return None;
    }
    Some(n as u32)
}

pub(super) fn jsoup_preorder_index(doc: &Html, el: &ElementRef<'_>) -> usize {
    let target = el.id();
    doc.tree
        .root()
        .descendants()
        .enumerate()
        .find(|(_, n)| n.id() == target)
        .map(|(i, _)| i)
        .unwrap_or(0)
}

pub(super) fn jsoup_element_at(doc: &Html, index: usize) -> Option<ElementRef<'_>> {
    doc.tree
        .root()
        .descendants()
        .nth(index)
        .and_then(ElementRef::wrap)
}

pub(super) fn jsoup_add_handle(doc: usize, node: usize) -> u32 {
    JSOUP_REG.with(|r| {
        let mut r = r.borrow_mut();
        r.handles.push((doc, node));
        r.handles.len() as u32
    })
}

/// 解析 jsoup 选择器；jsoup 专有扩展伪类由 Rust 侧统一处理，`:matches`/`:nth`
/// 这类尚未实现的形态显式报“未支持”，不静默降级。
pub(super) struct JsoupPlan {
    base: Selector,
    filters: Vec<JqFilter>,
}

pub(super) fn jsoup_plan(text: &str) -> Result<JsoupPlan, String> {
    let s = text.trim();
    if s.is_empty() {
        return Err("空选择器".to_string());
    }
    if s.contains(":nth(") {
        return Err(format!(
            "选择器使用了 jsoup 专有扩展伪类 :nth(…，当前后端(scraper)不支持：{s}"
        ));
    }
    let (base, filters) = split_jquery_filters(s);
    let base = if base.is_empty() {
        "*".to_string()
    } else {
        base
    };
    let base = Selector::parse(&base).map_err(|e| format!("CSS 选择器不合法：{s}（{e:?}）"))?;
    Ok(JsoupPlan { base, filters })
}

pub(super) fn jsoup_with_handle<R>(h: u32, f: impl FnOnce(&Html, &str, usize) -> R) -> Option<R> {
    JSOUP_REG.with(|r| {
        let r = r.borrow();
        let (doc, node) = *r.handles.get((h as usize).checked_sub(1)?)?;
        let d = r.docs.get(doc)?;
        Some(f(&d.html, &d.base, node))
    })
}

pub(super) fn jsoup_select_handles(h: u32, plan: &JsoupPlan) -> Vec<u32> {
    let (doc_idx, nodes) = JSOUP_REG.with(|r| {
        let r = r.borrow();
        let Some((doc, node)) = (h as usize)
            .checked_sub(1)
            .and_then(|i| r.handles.get(i))
            .copied()
        else {
            return (usize::MAX, Vec::new());
        };
        let Some(d) = r.docs.get(doc) else {
            return (usize::MAX, Vec::new());
        };
        let mut out = Vec::new();
        if node == 0 {
            let els: Vec<_> = d.html.select(&plan.base).collect();
            for el in apply_jq_filters(els, &plan.filters) {
                out.push(jsoup_preorder_index(&d.html, &el));
            }
        } else if let Some(el) = jsoup_element_at(&d.html, node) {
            let els: Vec<_> = el.select(&plan.base).collect();
            for e in apply_jq_filters(els, &plan.filters) {
                out.push(jsoup_preorder_index(&d.html, &e));
            }
        }
        (doc, out)
    });
    if doc_idx == usize::MAX {
        return Vec::new();
    }
    nodes
        .into_iter()
        .map(|n| jsoup_add_handle(doc_idx, n))
        .collect()
}

pub(super) fn jsoup_error(msg: &str) -> boa_engine::JsError {
    ajax_error(&format!("{UNSUPPORTED_JS_API}{msg}"))
}

pub(super) fn native_jsoup_parse(
    _this: &boa_engine::JsValue,
    args: &[boa_engine::JsValue],
    _ctx: &mut boa_engine::Context,
) -> boa_engine::JsResult<boa_engine::JsValue> {
    let html = jsoup_arg_str(args, 0);
    let base = jsoup_arg_str(args, 1);
    let doc = Html::parse_document(&html);
    let doc_idx = JSOUP_REG.with(|r| {
        let mut r = r.borrow_mut();
        if r.docs.len() >= JSOUP_MAX_DOCS {
            // 超过上限直接整体重置：句柄只在同一次求值内有效，重置是安全的上界保护。
            r.docs.clear();
            r.handles.clear();
        }
        r.docs.push(JsoupDoc { html: doc, base });
        r.docs.len() - 1
    });
    let h = jsoup_add_handle(doc_idx, 0);
    Ok(boa_engine::JsValue::from(h as f64))
}

pub(super) fn native_jsoup_select(
    _this: &boa_engine::JsValue,
    args: &[boa_engine::JsValue],
    _ctx: &mut boa_engine::Context,
) -> boa_engine::JsResult<boa_engine::JsValue> {
    let h = jsoup_arg_handle(args, 0)
        .ok_or_else(|| jsoup_error("org.jsoup.select（无效的文档/元素句柄）"))?;
    let sel_text = jsoup_arg_str(args, 1);
    let plan = jsoup_plan(&sel_text).map_err(|e| jsoup_error(&format!("org.jsoup select：{e}")))?;
    let handles = jsoup_select_handles(h, &plan);
    let json = serde_json::to_string(&handles).unwrap_or_else(|_| "[]".into());
    Ok(boa_engine::JsValue::from(boa_engine::JsString::from(json)))
}

pub(super) fn native_jsoup_attr(
    _this: &boa_engine::JsValue,
    args: &[boa_engine::JsValue],
    _ctx: &mut boa_engine::Context,
) -> boa_engine::JsResult<boa_engine::JsValue> {
    let h = jsoup_arg_handle(args, 0)
        .ok_or_else(|| jsoup_error("org.jsoup.attr（无效的文档/元素句柄）"))?;
    let key = jsoup_arg_str(args, 1);
    let v = jsoup_with_handle(h, |html, base, node| {
        let el = jsoup_element_at(html, node)?;
        // jsoup 的 `abs:<attr>` 前缀：先取真实属性，再按文档 baseUri 解析相对地址。
        // `abs:`（无属性名）等价于 `abs:href`。
        let (name, absolute) = match key.strip_prefix("abs:") {
            Some("") => ("href", true),
            Some(rest) => (rest, true),
            None => (key.as_str(), false),
        };
        let v = el.value().attr(name).unwrap_or("");
        let out = if absolute {
            if v.is_empty() {
                String::new()
            } else {
                let base = if base.is_empty() {
                    el.value().attr("base").unwrap_or("")
                } else {
                    base
                };
                url::Url::parse(base)
                    .ok()
                    .and_then(|b| b.join(v).ok())
                    .map(|u| u.to_string())
                    .unwrap_or_else(|| v.to_string())
            }
        } else {
            v.to_string()
        };
        Some(out)
    });
    match v {
        Some(Some(s)) => Ok(boa_engine::JsValue::from(boa_engine::JsString::from(s))),
        _ => Ok(boa_engine::JsValue::from(boa_engine::JsString::from(""))),
    }
}

pub(super) fn native_jsoup_text(
    _this: &boa_engine::JsValue,
    args: &[boa_engine::JsValue],
    _ctx: &mut boa_engine::Context,
) -> boa_engine::JsResult<boa_engine::JsValue> {
    let h = jsoup_arg_handle(args, 0)
        .ok_or_else(|| jsoup_error("org.jsoup.text（无效的文档/元素句柄）"))?;
    let out = jsoup_with_handle(h, |html, _base, node| {
        let el = jsoup_element_at(html, node)?;
        Some(jsoup_normalized_text(&el))
    })
    .flatten()
    .unwrap_or_default();
    Ok(boa_engine::JsValue::from(boa_engine::JsString::from(out)))
}

pub(super) fn native_jsoup_html(
    _this: &boa_engine::JsValue,
    args: &[boa_engine::JsValue],
    _ctx: &mut boa_engine::Context,
) -> boa_engine::JsResult<boa_engine::JsValue> {
    let h = jsoup_arg_handle(args, 0)
        .ok_or_else(|| jsoup_error("org.jsoup.html（无效的文档/元素句柄）"))?;
    let out = jsoup_with_handle(h, |html, _base, node| {
        let el = jsoup_element_at(html, node)?;
        Some(el.inner_html())
    })
    .flatten()
    .unwrap_or_default();
    Ok(boa_engine::JsValue::from(boa_engine::JsString::from(out)))
}

pub(super) fn native_jsoup_outer_html(
    _this: &boa_engine::JsValue,
    args: &[boa_engine::JsValue],
    _ctx: &mut boa_engine::Context,
) -> boa_engine::JsResult<boa_engine::JsValue> {
    let h = jsoup_arg_handle(args, 0)
        .ok_or_else(|| jsoup_error("org.jsoup.outerHtml（无效的文档/元素句柄）"))?;
    let out = jsoup_with_handle(h, |html, _base, node| {
        let el = jsoup_element_at(html, node)?;
        Some(el.html())
    })
    .flatten()
    .unwrap_or_default();
    Ok(boa_engine::JsValue::from(boa_engine::JsString::from(out)))
}

/// jsoup `text()`：拼接文本节点后把连续空白折叠为单个空格并去首尾（含块级换行近似）。
pub(super) fn jsoup_normalized_text(el: &ElementRef<'_>) -> String {
    let raw: String = el.text().collect();
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(super) fn native_jsoup_has_attr(
    _this: &boa_engine::JsValue,
    args: &[boa_engine::JsValue],
    _ctx: &mut boa_engine::Context,
) -> boa_engine::JsResult<boa_engine::JsValue> {
    let h = jsoup_arg_handle(args, 0)
        .ok_or_else(|| jsoup_error("org.jsoup.hasAttr（无效的文档/元素句柄）"))?;
    let key = jsoup_arg_str(args, 1);
    let has = jsoup_with_handle(h, |html, _base, node| {
        jsoup_element_at(html, node).map(|el| el.value().attr(&key).is_some())
    })
    .flatten()
    .unwrap_or(false);
    Ok(boa_engine::JsValue::from(has))
}

pub(super) fn native_jsoup_parent(
    _this: &boa_engine::JsValue,
    args: &[boa_engine::JsValue],
    _ctx: &mut boa_engine::Context,
) -> boa_engine::JsResult<boa_engine::JsValue> {
    let h = jsoup_arg_handle(args, 0)
        .ok_or_else(|| jsoup_error("org.jsoup.parent（无效的文档/元素句柄）"))?;
    let parent = JSOUP_REG.with(|r| {
        let r = r.borrow();
        let (doc, node) = *r.handles.get((h as usize).checked_sub(1)?)?;
        let html = &r.docs.get(doc)?.html;
        let parent = jsoup_element_at(html, node)?
            .parent()
            .and_then(ElementRef::wrap)?;
        Some((doc, jsoup_preorder_index(html, &parent)))
    });
    match parent {
        Some((doc, node)) => Ok(boa_engine::JsValue::from(jsoup_add_handle(doc, node) as f64)),
        None => Ok(boa_engine::JsValue::undefined()),
    }
}

pub(super) fn native_jsoup_next_siblings(
    _this: &boa_engine::JsValue,
    args: &[boa_engine::JsValue],
    _ctx: &mut boa_engine::Context,
) -> boa_engine::JsResult<boa_engine::JsValue> {
    let h = jsoup_arg_handle(args, 0)
        .ok_or_else(|| jsoup_error("org.jsoup.nextSiblingElements（无效句柄）"))?;
    let (doc_idx, nodes) = JSOUP_REG.with(|r| {
        let r = r.borrow();
        let Some((d, node)) = (h as usize)
            .checked_sub(1)
            .and_then(|i| r.handles.get(i))
            .copied()
        else {
            return (usize::MAX, Vec::new());
        };
        let mut out = Vec::new();
        if let Some(el) = jsoup_element_at(&r.docs[d].html, node) {
            for e in el.next_siblings().filter_map(ElementRef::wrap) {
                out.push(jsoup_preorder_index(&r.docs[d].html, &e));
            }
        }
        (d, out)
    });
    let handles: Vec<u32> = nodes
        .into_iter()
        .map(|n| jsoup_add_handle(doc_idx, n))
        .collect();
    let json = serde_json::to_string(&handles).unwrap_or_else(|_| "[]".into());
    Ok(boa_engine::JsValue::from(boa_engine::JsString::from(json)))
}

pub(super) fn native_jsoup_is(
    _this: &boa_engine::JsValue,
    args: &[boa_engine::JsValue],
    _ctx: &mut boa_engine::Context,
) -> boa_engine::JsResult<boa_engine::JsValue> {
    let h = jsoup_arg_handle(args, 0)
        .ok_or_else(|| jsoup_error("org.jsoup.is（无效的文档/元素句柄）"))?;
    let sel_text = jsoup_arg_str(args, 1);
    let plan = jsoup_plan(&sel_text).map_err(|e| jsoup_error(&format!("org.jsoup is：{e}")))?;
    let matched = jsoup_with_handle(h, |html, _base, node| {
        jsoup_element_at(html, node).map(|el| {
            plan.base.matches(&el) && !apply_jq_filters(vec![el], &plan.filters).is_empty()
        })
    })
    .flatten()
    .unwrap_or(false);
    Ok(boa_engine::JsValue::from(matched))
}

pub(super) fn native_jsoup_tag(
    _this: &boa_engine::JsValue,
    args: &[boa_engine::JsValue],
    _ctx: &mut boa_engine::Context,
) -> boa_engine::JsResult<boa_engine::JsValue> {
    let h = jsoup_arg_handle(args, 0)
        .ok_or_else(|| jsoup_error("org.jsoup.tagName（无效的文档/元素句柄）"))?;
    let tag = jsoup_with_handle(h, |html, _base, node| {
        jsoup_element_at(html, node).map(|el| el.value().name().to_string())
    })
    .flatten()
    .unwrap_or_default();
    Ok(boa_engine::JsValue::from(boa_engine::JsString::from(tag)))
}

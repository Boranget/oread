//! rule_engine HTML 辅助的JSON 列表展开与分卷（move-only 从 rule_engine_html.rs 拆出）。

use super::*;

/// JS 返回的 JSON 数组展开为列表项（上游规则 `getStringList` 的 List 语义）。
///
/// `keep_objects=true`（目录）：对象保留为紧凑 JSON，供 `chapterName/chapterUrl` 逐项取字段；
/// `keep_objects=false`（图片等纯值）：对象取 `src/url/href`，缺则取文本字段。
///
/// 兼容 `[[title, url]]` 二元数组、嵌套数组、`[{header, list:[…]}]` 分卷结构；
/// 空项/缺字段项跳过并记一条 warn（不报错）。
/// 返回 `None` 表示不是 JSON 数组（保持原调用方的换行拆分逻辑）。
pub(in crate::rule_engine) fn expand_json_list_items(
    s: &str,
    keep_objects: bool,
) -> Option<Vec<String>> {
    let t = s.trim_start();
    if !t.starts_with('[') {
        return None;
    }
    let val: Value = serde_json::from_str(t).ok()?;
    let mut out = Vec::new();
    let mut dropped = 0usize;
    flatten_json_items(val, keep_objects, &mut out, &mut dropped, true);
    if dropped > 0 {
        tracing::warn!(dropped, keep_objects, "JSON 列表展开跳过空项/缺字段项");
    }
    Some(out)
}

pub(in crate::rule_engine) fn flatten_json_items(
    v: Value,
    keep: bool,
    out: &mut Vec<String>,
    dropped: &mut usize,
    top_level: bool,
) {
    match v {
        Value::Null => *dropped += 1,
        Value::String(s) => {
            if s.trim().is_empty() {
                *dropped += 1;
            } else {
                out.push(s);
            }
        }
        Value::Bool(b) => out.push(b.to_string()),
        Value::Number(n) => out.push(n.to_string()),
        Value::Array(arr) => {
            // `[[title, url]]` 里的二元数组：规范化为带别名的对象，后续字段规则可直接取值。
            // 顶层数组不按二元处理（`java.getStringList` 返回的 `['甲','乙']` 是两个列表项）。
            if !top_level
                && arr.len() == 2
                && !arr[0].is_array()
                && !arr[0].is_object()
                && !arr[1].is_array()
                && !arr[1].is_object()
            {
                let a = json_scalar_text(&arr[0]).trim().to_string();
                let b = json_scalar_text(&arr[1]).trim().to_string();
                if a.is_empty() && b.is_empty() {
                    *dropped += 1;
                } else if keep {
                    out.push(
                        serde_json::json!({
                            "text": a, "name": a, "title": a,
                            "href": b, "url": b
                        })
                        .to_string(),
                    );
                } else {
                    out.push(if b.is_empty() { a } else { b });
                }
            } else {
                for x in arr {
                    flatten_json_items(x, keep, out, dropped, false);
                }
            }
        }
        Value::Object(mut map) => {
            // 分卷结构 `{header, list:[…]}`：头项作分卷标记，子项递归展开
            if let Some(group) = take_group_list(&mut map) {
                let header = group_header(&map);
                if keep && !header.is_empty() {
                    out.push(
                        serde_json::json!({
                            "text": header, "name": header, "title": header,
                            "href": "", "url": "", "volume": true
                        })
                        .to_string(),
                    );
                }
                for x in group {
                    flatten_json_items(x, keep, out, dropped, false);
                }
            } else if keep {
                if map_has_chapter_field(&map) {
                    out.push(Value::Object(map).to_string());
                } else {
                    *dropped += 1;
                }
            } else {
                let pick = map_pick_url(&map);
                if pick.is_empty() {
                    *dropped += 1;
                } else {
                    out.push(pick);
                }
            }
        }
    }
}

/// 识别分卷：对象里第一个数组型分组字段（`list`/`chapters`/`items`…）。
pub(in crate::rule_engine) fn take_group_list(
    map: &mut serde_json::Map<String, Value>,
) -> Option<Vec<Value>> {
    for k in ["list", "chapters", "items", "chapterList", "chapter_list"] {
        if map.get(k).map(|v| v.is_array()).unwrap_or(false) {
            return match map.remove(k) {
                Some(Value::Array(a)) => Some(a),
                _ => None,
            };
        }
    }
    None
}

pub(in crate::rule_engine) fn group_header(map: &serde_json::Map<String, Value>) -> String {
    for k in ["header", "name", "title", "text", "chapterName"] {
        if let Some(v) = map.get(k) {
            let s = json_scalar_text(v);
            if !s.trim().is_empty() {
                return s;
            }
        }
    }
    String::new()
}

pub(in crate::rule_engine) fn map_has_chapter_field(map: &serde_json::Map<String, Value>) -> bool {
    // 字段名不做白名单：书源常用短名（`{n,u}`/`{t,h}`）或自定义字段，由后续字段规则取值；
    // 只要对象里至少有一个非空标量就保留，整项全空才按“缺字段”丢弃。
    map.values().any(|v| !json_scalar_text(v).trim().is_empty())
}

pub(in crate::rule_engine) fn map_pick_url(map: &serde_json::Map<String, Value>) -> String {
    for k in ["src", "url", "href", "link", "text", "name", "title"] {
        if let Some(v) = map.get(k) {
            let s = json_scalar_text(v);
            if !s.trim().is_empty() {
                return s.trim().to_string();
            }
        }
    }
    String::new()
}

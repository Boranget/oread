//! 通用 JSON 键名转换：Legado 书源导出使用 camelCase（bookSourceUrl / ruleSearch …），
//! 而内部实体为 snake_case。导入时递归把对象键转为 snake_case，使两者兼容。

use serde::{Deserialize, Deserializer};
use serde_json::Value;

/// 允许数字字段以字符串形式出现（Legado 某些导出把 i64 写为字符串）。
pub fn string_or_i64<'de, D>(d: D) -> Result<i64, D::Error>
where
    D: Deserializer<'de>,
{
    match Value::deserialize(d)? {
        Value::Number(n) => n
            .as_i64()
            .ok_or_else(|| serde::de::Error::custom("not an i64")),
        Value::String(s) => s.parse::<i64>().map_err(serde::de::Error::custom),
        _ => Err(serde::de::Error::custom(
            "expected number or numeric string",
        )),
    }
}

/// 允许布尔字段以字符串形式出现（"true"/"false"/"1"/"0"）。
pub fn string_or_bool<'de, D>(d: D) -> Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    match Value::deserialize(d)? {
        Value::Bool(b) => Ok(b),
        Value::String(s) => match s.to_ascii_lowercase().as_str() {
            "true" | "1" => Ok(true),
            "false" | "0" => Ok(false),
            _ => Err(serde::de::Error::custom("expected bool")),
        },
        _ => Err(serde::de::Error::custom("expected bool")),
    }
}

/// 递归把 JSON 中所有对象键从 camelCase 转为 snake_case。
pub fn camel_to_snake_value(v: Value) -> Value {
    match v {
        Value::Object(map) => {
            let mut new = serde_json::Map::new();
            for (k, val) in map {
                new.insert(camel_to_snake(&k), camel_to_snake_value(val));
            }
            Value::Object(new)
        }
        Value::Array(arr) => Value::Array(arr.into_iter().map(camel_to_snake_value).collect()),
        other => other,
    }
}

/// 递归把 JSON 中所有对象键从 snake_case 转回 camelCase（Legado 导出格式）。
pub fn snake_to_camel_value(v: Value) -> Value {
    match v {
        Value::Object(map) => {
            let mut new = serde_json::Map::new();
            for (k, val) in map {
                new.insert(snake_to_camel(&k), snake_to_camel_value(val));
            }
            Value::Object(new)
        }
        Value::Array(arr) => Value::Array(arr.into_iter().map(snake_to_camel_value).collect()),
        other => other,
    }
}

fn snake_to_camel(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut upper_next = false;
    for c in s.chars() {
        if c == '_' {
            upper_next = true;
        } else if upper_next {
            out.push(c.to_ascii_uppercase());
            upper_next = false;
        } else {
            out.push(c);
        }
    }
    out
}

fn camel_to_snake(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for (i, c) in s.chars().enumerate() {
        if c.is_uppercase() {
            if i != 0 {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_keys() {
        let raw = serde_json::json!({
            "bookSourceUrl": "http://x",
            "ruleSearch": { "bookList": ".x", "lastChapter": "y" }
        });
        let v = camel_to_snake_value(raw);
        assert_eq!(v["book_source_url"], "http://x");
        assert_eq!(v["rule_search"]["book_list"], ".x");
        assert_eq!(v["rule_search"]["last_chapter"], "y");
    }

    #[test]
    fn converts_keys_back_to_camel() {
        let raw = serde_json::json!({
            "book_source_url": "http://x",
            "rule_search": { "book_list": ".x", "check_key_word": "y" }
        });
        let v = snake_to_camel_value(raw);
        assert_eq!(v["bookSourceUrl"], "http://x");
        assert_eq!(v["ruleSearch"]["bookList"], ".x");
        assert_eq!(v["ruleSearch"]["checkKeyWord"], "y");
    }
}

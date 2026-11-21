//! AnalyzeRule 核心：规则解析与动作执行（move-only 从 rule_engine.rs 拆出）。
//!
//! 只搬移实现，不改规则语义；父模块保留 RuleError 与子模块入口。

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::mpsc::channel;

use regex::Regex;
use serde_json::Value;

use super::html::*;
use super::js::*;
use super::json_path::json_query;
use super::{Mode, RuleError};

pub struct AnalyzeRule {
    pub variables: RefCell<HashMap<String, String>>,
    /// 规则 JS 环境里的 `cache` 全局（由引擎注入缓存管理对象）。
    /// 最小等价版：同作用域可读写的字符串 map。上游 CacheManager 是 App 级单例 + SQLite，
    /// 本服务**每作用域一份**（随 AnalyzeRule 存活），不共享、不持久化。
    pub cache: RefCell<HashMap<String, String>>,
    /// 跨请求的 book 层变量（由引擎在求值之间透传）。
    /// `@put:{key:rule}` 除了写当前 `variables`，也写这里；路由层在请求结束后按
    /// `book_key` 落库，下一次同书请求（info → toc → content）预载回来。
    pub book_vars: RefCell<HashMap<String, String>>,
    /// 当前作用域所属的 book_key（由路由层/编排层设置，None 表示没有 book 维度）。
    pub book_key: RefCell<Option<String>>,
    pub js_lib: String,
    pub is_json: bool,
    /// 本次顶层规则求值的原始 content；供 `java.getElement`（jsoup 桥）在 JS 中取页面。
    pub current_content: RefCell<String>,
    /// 当前书源 key（`source.getKey()`），由编排层注入。
    pub source_key: RefCell<String>,
    /// JS 环境里的 `source.loginUrl`（登录源常用 `eval(String(source.loginUrl))`）。
    pub source_login_url: RefCell<String>,
    /// 本次 JS 是否调用了 `java.refreshTocUrl()`。
    pub refresh_toc: Cell<bool>,
    /// JS 环境里的 `book.bookUrl`（`pre_update_js` 可能改写，需回写编排层）。
    pub book_url: RefCell<String>,
    /// JS 环境里的 `book.tocUrl`。
    pub book_toc_url: RefCell<String>,
    /// JS 环境里的 `book.name/book.author/book.kind/book.intro/book.coverUrl`。
    /// 上游 `book` 绑定的是完整 Book 实体，规则（尤其简介模板）会直接读这些元信息；
    /// 编排层在详情/目录/正文阶段按当前 Book 值注入（搜索阶段为空）。
    pub book_meta: RefCell<BookMeta>,
}

/// `book` 绑定里的只读元信息（对应 书源对象 的同名属性）。
#[derive(Clone, Default)]
pub struct BookMeta {
    pub name: String,
    pub author: String,
    pub kind: String,
    pub intro: String,
    pub cover_url: String,
}

impl Default for AnalyzeRule {
    fn default() -> Self {
        Self {
            variables: RefCell::new(HashMap::new()),
            cache: RefCell::new(HashMap::new()),
            book_vars: RefCell::new(HashMap::new()),
            book_key: RefCell::new(None),
            js_lib: String::new(),
            is_json: false,
            current_content: RefCell::new(String::new()),
            source_key: RefCell::new(String::new()),
            source_login_url: RefCell::new(String::new()),
            refresh_toc: Cell::new(false),
            book_url: RefCell::new(String::new()),
            book_toc_url: RefCell::new(String::new()),
            book_meta: RefCell::new(BookMeta::default()),
        }
    }
}

impl AnalyzeRule {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_var(&self, k: impl Into<String>, v: impl Into<String>) {
        self.variables.borrow_mut().insert(k.into(), v.into());
    }

    /// 注入 `book.*` 只读元信息（详情/目录/正文阶段由编排层调用）。
    pub fn set_book_meta(&self, meta: BookMeta) {
        *self.book_meta.borrow_mut() = meta;
    }

    /// 执行一段独立 JS（`pre_update_js` 等编排层钩子）；`content` 作为 `result`/`__input`。
    pub fn eval_script(
        &self,
        code: &str,
        content: &str,
        base_url: &str,
    ) -> Result<String, RuleError> {
        self.eval_js(content, code, base_url)
    }

    /// 图片解密（`ContentRule.image_decode`）：`result`/`bytes` 为图片原始字节，`src` 为图片地址。
    /// 返回解密后的字节；JS 抛错由调用方回退原图并记规则异常。
    pub fn eval_image_decode(
        &self,
        code: &str,
        bytes: &[u8],
        src: &str,
    ) -> Result<Vec<u8>, RuleError> {
        let json = serde_json::to_string(bytes)
            .map_err(|e| RuleError::Other(format!("image bytes 序列化失败：{e}")))?;
        let wrapped = format!(
            "var result=JSON.parse(result); var bytes=result; var src=String(__base);\n{code}"
        );
        let out = self.eval_js(&json, &wrapped, src)?;
        let t = out.trim();
        if t.starts_with('[') {
            let v: Vec<i64> = serde_json::from_str(t)
                .map_err(|e| RuleError::Other(format!("image_decode 未返回字节数组：{e}")))?;
            Ok(v.into_iter().map(|n| n.clamp(0, 255) as u8).collect())
        } else {
            // Rhino 里解图脚本也可能返回二进制字符串：按 charCode 逐字节还原
            Ok(t.chars()
                .map(|c| c as u32)
                .filter(|n| *n <= 255)
                .map(|n| n as u8)
                .collect())
        }
    }

    /// 批量注入变量；生产路径逐条调用 `set_var`，本方法仅单测使用。
    #[allow(dead_code)]
    pub fn set_vars(&self, m: HashMap<String, String>) {
        self.variables.borrow_mut().extend(m);
    }

    /// 进入一本书的作用域：清空请求内变量，载入该书持久化的变量与 cache 快照。
    /// 之后 `@put` 同时写入 `variables`（本请求可见）与 `book_vars`（请求结束落库）。
    pub fn begin_book_scope(
        &self,
        key: String,
        vars: HashMap<String, String>,
        cache: HashMap<String, String>,
    ) {
        let mut v = self.variables.borrow_mut();
        v.clear();
        v.extend(vars.iter().map(|(k, val)| (k.clone(), val.clone())));
        self.book_vars.replace(vars);
        self.cache.replace(cache);
        self.book_key.replace(Some(key));
    }

    pub fn book_scope_key(&self) -> Option<String> {
        self.book_key.borrow().clone()
    }

    pub fn book_vars_snapshot(&self) -> HashMap<String, String> {
        self.book_vars.borrow().clone()
    }

    pub fn cache_snapshot(&self) -> HashMap<String, String> {
        self.cache.borrow().clone()
    }
}

#[path = "rule_engine_core_get.rs"]
mod get;
#[path = "rule_engine_core_js.rs"]
mod js;
#[path = "rule_engine_core_lists.rs"]
mod lists;
#[path = "rule_engine_core_rules.rs"]
mod rules;
#[path = "rule_engine_core_vars.rs"]
mod vars;

/// 生成安全的 JS 字符串字面量（复用 serde_json 转义，并处理 JS 行分隔符）。
fn js_string_literal(s: &str) -> String {
    serde_json::to_string(s)
        .unwrap_or_else(|_| "\"\"".to_string())
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029")
}

/// 把 JSON 元素转成纯文本：字符串取内部值（不含 JSON 引号），其余 JSON 序列化。
fn value_to_string(v: Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        other => other.to_string(),
    }
}

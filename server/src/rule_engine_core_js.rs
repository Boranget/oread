//! AnalyzeRule 的JS 求值与 DOM 桥（move-only 从 rule_engine_core.rs 拆出）。

use super::*;

impl AnalyzeRule {
    /// DOM 桥 Phase 1：按规则里出现的字面量选择器，在当前页面 HTML 上预抽取。
    /// 返回 (`getElements`/`getStringList` 的 HTML 列表映射, `getString` 的单值映射)。
    pub(super) fn dom_bridge(
        &self,
        input: &str,
        code: &str,
        base_url: &str,
    ) -> (
        serde_json::Map<String, serde_json::Value>,
        serde_json::Map<String, serde_json::Value>,
    ) {
        let mut lists = serde_json::Map::new();
        let mut singles = serde_json::Map::new();
        // 页面超过上限不做预抽取：垫片会返回“仅支持字面量选择器”的具名提示
        if input.len() > DOM_MAX_INPUT_BYTES {
            return (lists, singles);
        }
        let mut total_bytes = 0usize;
        for (kind, sel) in scan_dom_calls(code) {
            let sel = sel.trim().to_string();
            // 选择器自身含 JS 段时不预处理，避免递归
            if sel.is_empty() || sel.contains("@js:") || sel.contains("<js>") {
                continue;
            }
            if lists.contains_key(&sel) || singles.contains_key(&sel) {
                continue;
            }
            if kind == "getString" {
                if let Ok(v) = self.get_string(input, &sel, false, base_url) {
                    singles.insert(sel, serde_json::Value::String(v));
                }
                continue;
            }
            let Ok(mut vals) = self.get_string_list(input, &sel) else {
                continue;
            };
            vals.truncate(DOM_MAX_NODES);
            total_bytes += vals.iter().map(|v| v.len()).sum::<usize>();
            if total_bytes > DOM_MAX_INPUT_BYTES {
                break;
            }
            lists.insert(
                sel,
                serde_json::Value::Array(vals.into_iter().map(serde_json::Value::String).collect()),
            );
        }
        (lists, singles)
    }

    /// 把规则变量注入 JS 作用域（等价于 Legado evalJS 前绑定 page/title 等变量）。
    pub(super) fn js_vars(&self) -> String {
        const RESERVED: &[&str] = &[
            "result",
            "__input",
            "__base",
            "baseUrl",
            "__DOM",
            "__DOMS",
            "__r",
            "__content",
            "__nativeSelect",
            "__nativeAjax",
            "__nativeSourceVarGet",
            "__nativeSourceVarPut",
            "__nativeCacheFileGet",
            "__nativeCacheFilePut",
            "java",
            "cache",
            "__cache",
            "__out",
            "cookie",
            "source",
            "book",
        ];
        let mut out = String::new();
        for (k, v) in self.variables.borrow().iter() {
            let mut chars = k.chars();
            let ok_first = chars
                .next()
                .map(|c| c.is_ascii_alphabetic() || c == '_' || c == '$')
                .unwrap_or(false);
            if !ok_first
                || !k
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
                || RESERVED.contains(&k.as_str())
            {
                continue;
            }
            out.push_str("var ");
            out.push_str(k);
            out.push('=');
            out.push_str(&js_string_literal(v));
            out.push_str(";\n");
        }
        out
    }

    pub(in crate::rule_engine) fn eval_js(
        &self,
        input: &str,
        code: &str,
        base_url: &str,
    ) -> Result<String, RuleError> {
        let lit = js_string_literal(input);
        self.eval_js_lit(&lit, input, code, base_url)
    }

    /// 列表语义的 JS 求值：`result` 绑定为 JS 数组（Legado `getStringList` 的
    /// `evalJS(rule, List)`，见 AnalyzeRule.getStringList：Default 段先出 List<String>）。
    pub(super) fn eval_js_list(
        &self,
        items: &[String],
        code: &str,
        base_url: &str,
    ) -> Result<String, RuleError> {
        let lit = serde_json::to_string(items).unwrap_or_else(|_| "[]".into());
        let joined = items.join("\n");
        self.eval_js_lit(&lit, &joined, code, base_url)
    }

    /// `lit` 是注入到 `var result=` 的 JS 字面量（单值为带引号字符串，列表为数组字面量）。
    pub(super) fn eval_js_lit(
        &self,
        lit: &str,
        input: &str,
        code: &str,
        base_url: &str,
    ) -> Result<String, RuleError> {
        let conf = js_conf();
        if !conf.enabled {
            return Err(RuleError::Other("JS rules disabled by JS_ENABLED=0".into()));
        }
        // 规则级 `{{…}}` 模板先于脚本展开（Legado `makeUpRule` 的模板语义），
        // 覆盖 `@js:` 与 `<js>` 段，也覆盖 searchUrl 的 `@js:` 分支（p11 根因）。
        let code = &self.expand_js_template(code, input);
        // 整段包成 IIFE，避免不同书源/不同次执行之间的全局变量污染
        let shim = format!(
            "{}\n{}\n{}\n{}\n{}",
            CN_TO_NUM_JS, JS_UTF8_HELPER, JS_MD5, JS_T2S, JAVA_BOOK_SHIM
        );
        // DOM 桥 Phase 1：把规则里“字面量选择器”的求值结果预抽取成 JSON 映射，
        // 注入到同一个 IIFE 作用域；JS 里 java.getElements 只做查表，不再抛错。
        let (dom, doms) = self.dom_bridge(input, code, base_url);
        let dom_js = serde_json::to_string(&dom).unwrap_or_else(|_| "{}".into());
        let doms_js = serde_json::to_string(&doms).unwrap_or_else(|_| "{}".into());
        let vars_js = self.js_vars();
        let cache_init =
            serde_json::to_string(&self.cache.borrow().clone()).unwrap_or_else(|_| "{}".into());
        // `java.put` / `book.putVariable` 写入的变量表要在本次求值开始时就可见
        // （`@put` 在规则层先执行，JS 里 `java.get` 应能读到）。
        let vars_init =
            serde_json::to_string(&self.variables.borrow().clone()).unwrap_or_else(|_| "{}".into());
        let script = format!(
            "(function(){{ var result={r}; var __input={r}; var __base={b}; var baseUrl=__base;\nvar __page={page}; var __sourceKey={sk}; var __sourceLoginUrl={lu}; var __refreshToc=false; var __bookUrl={bu}; var __bookTocUrl={bt};\nvar __bookName={bn}; var __bookAuthor={ba}; var __bookKind={bk}; var __bookIntro={bi}; var __bookCoverUrl={bc};\n{vars}\n{cachejs}\n{shim}\n{lib}\n__vars={varsinit};\n__cache={cacheinit};\n__DOM={dom};\n__DOMS={doms};\nvar __t0 = Date.now();\nvar __r = eval({code});\nvar __ms = Date.now() - __t0;\nvar __out;\nif (Array.isArray(__r)) {{ try {{ __out = JSON.stringify(__r); }} catch(__e) {{}} }}\nelse __out = String(__r);\nif (typeof __out === 'undefined') __out = String(__r);\nreturn __out + {vsent} + JSON.stringify(__vars) + {sent} + JSON.stringify(__cache) + {book} + JSON.stringify({{bookUrl: book.bookUrl, tocUrl: book.tocUrl}}) + {tocs} + (__refreshToc ? '1' : '0') + {msent} + String(__ms); }})()",
            r = lit,
            b = js_string_literal(base_url),
            page = js_string_literal(&self.current_content.borrow()),
            sk = js_string_literal(&self.source_key.borrow()),
            lu = js_string_literal(&self.source_login_url.borrow()),
            bu = js_string_literal(&self.book_url.borrow()),
            bt = js_string_literal(&self.book_toc_url.borrow()),
            bn = js_string_literal(&self.book_meta.borrow().name),
            ba = js_string_literal(&self.book_meta.borrow().author),
            bk = js_string_literal(&self.book_meta.borrow().kind),
            bi = js_string_literal(&self.book_meta.borrow().intro),
            bc = js_string_literal(&self.book_meta.borrow().cover_url),
            vars = vars_js,
            varsinit = vars_init,
            cachejs = JS_CACHE,
            // jsLib 是共享作用域脚本，必须排在 shim 之后求值（见 js_lib_preload_js）。
            lib = js_lib_preload_js(&js_lib_urls(&self.js_lib)),
            shim = shim,
            cacheinit = cache_init,
            dom = dom_js,
            doms = doms_js,
            code = js_string_literal(code),
            vsent = js_string_literal(JS_VARS_SENTINEL),
            sent = js_string_literal(JS_CACHE_SENTINEL),
            book = js_string_literal(JS_BOOK_SENTINEL),
            tocs = js_string_literal(JS_REFRESH_TOC_SENTINEL),
            msent = js_string_literal(JS_RULE_MS_SENTINEL),
        );
        let (tx, rx) = channel::<(Result<String, String>, u64, u64)>();
        // 慢 JS 只阻塞本次规则求值，不再占住 tokio worker；在途超限则快速降级（见 `run_js_job`）。
        let (result, net_ms, js_elapsed) = run_js_job(JsJob { script, reply: tx }, rx)?;
        // 结果串尾部依次带回 cache 变更与规则自身耗时（非法/超大都忽略，不影响主结果）
        let mut rule_ms: Option<u64> = None;
        let result = match result {
            Ok(mut s) => {
                if let Some(idx) = s.rfind(JS_RULE_MS_SENTINEL) {
                    let tail = s[idx + JS_RULE_MS_SENTINEL.len()..].trim().to_string();
                    if let Ok(ms) = tail.parse::<u64>() {
                        rule_ms = Some(ms);
                    }
                    s.truncate(idx);
                }
                if let Some(idx) = s.rfind(JS_REFRESH_TOC_SENTINEL) {
                    let flag = s[idx + JS_REFRESH_TOC_SENTINEL.len()..].trim();
                    if flag.starts_with('1') {
                        self.refresh_toc.set(true);
                    }
                    s.truncate(idx);
                }
                // `pre_update_js` 可能改写 book.bookUrl / book.tocUrl，回写到规则上下文
                if let Some(idx) = s.rfind(JS_BOOK_SENTINEL) {
                    let tail = s[idx + JS_BOOK_SENTINEL.len()..].trim().to_string();
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&tail) {
                        if let Some(u) = v.get("bookUrl").and_then(|x| x.as_str()) {
                            if !u.is_empty() {
                                self.book_url.replace(u.to_string());
                            }
                        }
                        if let Some(u) = v.get("tocUrl").and_then(|x| x.as_str()) {
                            if !u.is_empty() {
                                self.book_toc_url.replace(u.to_string());
                            }
                        }
                    }
                    s.truncate(idx);
                }
                if let Some(idx) = s.rfind(JS_CACHE_SENTINEL) {
                    let tail = s[idx + JS_CACHE_SENTINEL.len()..].to_string();
                    if tail.len() <= 1_048_576 {
                        if let Ok(map) = serde_json::from_str::<HashMap<String, String>>(&tail) {
                            self.cache.replace(map);
                        }
                    }
                    s.truncate(idx);
                }
                // JS 侧 `java.put` / `book.putVariable` / `java.putVariable` 的写入回填到
                // 规则变量表：本请求后续规则（含 `@get:{key}`）立即可见；book 层同步写
                // `book_vars`，由编排层按 book_key 落库，与 Legado `book.putVariable` 一致。
                if let Some(idx) = s.rfind(JS_VARS_SENTINEL) {
                    let tail = s[idx + JS_VARS_SENTINEL.len()..].to_string();
                    if tail.len() <= 1_048_576 {
                        if let Ok(map) = serde_json::from_str::<HashMap<String, String>>(&tail) {
                            let mut vars = self.variables.borrow_mut();
                            for (k, v) in map {
                                vars.insert(k.clone(), v.clone());
                                self.book_vars.borrow_mut().insert(k, v);
                            }
                        }
                    }
                    s.truncate(idx);
                }
                Ok(s)
            }
            Err(e) => Err(e),
        };
        let (result, net_ms) = (result.map_err(RuleError::Other), net_ms);
        // 预算只算规则自身耗时（页面里 Date.now 包裹 eval 的那段），固定垫片/lib 的
        // 解析成本与机器负载相关，不该算到源作者头上；`java.ajax` 联网时间也扣掉。
        // 拿不到回传耗时（脚本抛错等）时退回整段耗时，保守但不会放过慢规则。
        let budget_ms = rule_ms.unwrap_or(js_elapsed).saturating_sub(net_ms);
        // 排队等单一 JS 线程的时间不计入预算：并发测试/并发请求下同一规则仍应通过。
        JS_LAST_MS.store(js_elapsed, Ordering::Relaxed);
        JS_MAX_MS.fetch_max(budget_ms, Ordering::Relaxed);
        // 已经报错的脚本保留原始错误（如 RuntimeLimit），不要被“超预算”盖掉
        if result.is_ok() && budget_ms > conf.timeout_ms {
            tracing::warn!(
                elapsed_ms = js_elapsed,
                rule_ms = budget_ms,
                net_ms,
                budget_ms = conf.timeout_ms,
                "js rule exceeded time budget"
            );
            return Err(RuleError::Other(format!(
                "js eval exceeded {}ms budget (took {}ms)",
                conf.timeout_ms, budget_ms
            )));
        }
        result
    }
}

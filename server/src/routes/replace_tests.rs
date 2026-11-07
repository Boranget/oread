//! 替换规则路由单测（move-only 从 replace.rs 拆出）。

use super::*;

fn mk(name: &str, pat: &str, repl: &str, regex: bool) -> ReplaceRule {
    ReplaceRule {
        id: None,
        name: Some(name.into()),
        group: None,
        pattern: pat.into(),
        replacement: repl.into(),
        scope: None,
        scope_title: true,
        scope_content: true,
        exclude_scope: None,
        is_enabled: true,
        is_regex: regex,
        timeout_millisecond: 1000,
        order: 0,
    }
}

// 内置净化：整行广告删除，句中插句（“武动乾坤”）必须保留，不能误删正文
#[test]
fn builtin_clean_removes_ad_lines_but_keeps_body() {
    let text = "\n\t\t一秒记住【顶点小说】 www.dingdian888.com，更新快，无弹窗！\n\t\t痛！好痛！\n本章未完请点击下一页继续阅读\n月如银盘，漫天繁星武动乾坤。\n武动乾坤\n";
    let out = apply_rules(&builtin_rules(), "", "", text, false);
    assert!(!out.contains("一秒记住"), "站点提示行应删除: {out}");
    assert!(!out.contains("dingdian888"), "域名广告应删除: {out}");
    assert!(!out.contains("本章未完"), "翻页引导应删除: {out}");
    assert!(
        out.contains("月如银盘，漫天繁星武动乾坤。"),
        "句中出现书名是正文，不能误删: {out}"
    );
    let lines: Vec<&str> = out
        .lines()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    assert_eq!(
        lines,
        vec!["痛！好痛！", "月如银盘，漫天繁星武动乾坤。"],
        "仅独立成行的书名推广应删除"
    );
}

// 源站反爬提示：常见变体整句删除；正文恰好含“阅读模式”四字时不能误删
#[test]
fn builtin_clean_removes_anticrawl_tip_sentence() {
    let positives = [
        "阅读提示：为防止内容获取不全，请勿使用浏览器阅读模式。",
        "阅读提示：请勿使用阅读模式。",
        "为防止内容不全，请勿使用浏览器阅读模式！",
    ];
    for line in positives {
        let out = apply_rules(&builtin_rules(), "", "", line, false);
        assert_eq!(out.trim(), "", "反爬提示应整句删除: {line} -> {out}");
    }

    let text = "阅读提示：为防止内容获取不全，请勿使用浏览器阅读模式。\n\
               他研究了很久的阅读模式。\n\
               作者在文中谈到阅读模式，请勿错过。\n\
               为防止内容不全，编辑补充了后文。";
    let out = apply_rules(&builtin_rules(), "", "", text, false);
    assert!(!out.contains("阅读提示"), "反爬提示句应删除: {out}");
    let lines: Vec<&str> = out
        .lines()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    assert_eq!(
        lines,
        vec![
            "他研究了很久的阅读模式。",
            "作者在文中谈到阅读模式，请勿错过。",
            "为防止内容不全，编辑补充了后文。"
        ],
        "正文不能误删: {out}"
    );
}

async fn settings_pool_with_line_height(v: &str) -> SqlitePool {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::query("CREATE TABLE user_settings (key TEXT PRIMARY KEY, value TEXT)")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO user_settings (key,value) VALUES ('line_height', ?1)")
        .bind(v)
        .execute(&pool)
        .await
        .unwrap();
    pool
}

// 旧默认行距 1.8 只归一化一次；其它值（1.7/1.75/1.9/2.0）一律不动
#[tokio::test]
async fn legacy_line_height_migrates_once_and_only_exact_default() {
    let p18 = settings_pool_with_line_height("1.8").await;
    assert_eq!(migrate_legacy_line_height_once(&p18).await.unwrap(), 1);
    let got: String = sqlx::query_scalar("SELECT value FROM user_settings WHERE key='line_height'")
        .fetch_one(&p18)
        .await
        .unwrap();
    assert_eq!(got, "1.75");
    let marked: Option<String> = sqlx::query_scalar("SELECT value FROM user_settings WHERE key=?1")
        .bind(DEFAULTS_MIGRATED_KEY)
        .fetch_optional(&p18)
        .await
        .unwrap();
    assert!(marked.is_some(), "迁移后应写入 defaults_migrated_v1 标记");
    assert_eq!(
        migrate_legacy_line_height_once(&p18).await.unwrap(),
        0,
        "标记生效后第二次执行 rows_affected 应为 0"
    );

    for v in ["1.7", "1.75", "1.9", "2.0"] {
        let p = settings_pool_with_line_height(v).await;
        assert_eq!(
            migrate_legacy_line_height_once(&p).await.unwrap(),
            0,
            "{v} 不是旧默认值，不应被改"
        );
        let keep: String =
            sqlx::query_scalar("SELECT value FROM user_settings WHERE key='line_height'")
                .fetch_one(&p)
                .await
                .unwrap();
        assert_eq!(keep, v);
    }
}

// 内置规则必须全部是合法正则，且默认关闭（开关在 /api/settings）
#[test]
fn builtin_clean_rules_compile_and_default_off() {
    let rules = builtin_rules();
    assert!(rules.len() >= 5);
    for r in &rules {
        assert!(
            regex::Regex::new(&r.pattern).is_ok(),
            "非法正则: {}",
            r.pattern
        );
        assert!(r.is_enabled && r.scope_content && !r.scope_title);
    }
    assert!(SETTINGS_DEFAULTS
        .iter()
        .any(|(k, v)| *k == "builtin_clean" && *v == "0"));
}

// auto_refresh 不能是假开关：设置可写，且书架页必须真的消费它（并有节流）
#[test]
fn auto_refresh_has_a_real_consumer() {
    assert!(SETTINGS_DEFAULTS
        .iter()
        .any(|(k, v)| *k == "auto_refresh" && *v == "1"));
    let bookshelf = include_str!("../../web/src/pages/Bookshelf.svelte");
    assert!(
        bookshelf.contains("/settings") || bookshelf.contains("api.settings"),
        "书架页应读取 /api/settings"
    );
    assert!(
        bookshelf.contains("auto_refresh"),
        "书架页未消费 auto_refresh，开关仍是假的"
    );
    assert!(
        bookshelf.contains("oread_autorefresh_last"),
        "自动刷新必须有 24h 节流键，避免每次打开都刷"
    );
    assert!(
        bookshelf.contains("enable_update !== false"),
        "自动刷新只应作用于未停更的书"
    );
}

// 导出为 Legado 原生 camelCase，且不带本地自增 id
#[test]
fn export_uses_legado_camel_case_and_drops_id() {
    let mut r = mk("去广告", "广告", "", false);
    r.id = Some(7);
    r.scope = Some("某书".into());
    let v = rule_to_legado_value(&r);
    assert!(v.get("id").is_none());
    assert_eq!(v["name"], "去广告");
    assert_eq!(v["isRegex"], false);
    assert_eq!(v["isEnabled"], true);
    assert_eq!(v["scopeTitle"], true);
    assert_eq!(v["scopeContent"], true);
    assert_eq!(v["timeoutMillisecond"], 1000);
    assert_eq!(v["scope"], "某书");
}

// 导入 Legado 字段：缺省 isEnabled=true、作用域两者都缺省时置 true、timeout 补 1000
#[test]
fn import_applies_legado_defaults() {
    let r = normalize_import_rule(serde_json::json!({
        "name": "去广告",
        "pattern": "广告",
        "replacement": "",
        "isRegex": false,
        "scope": "某书"
    }))
    .unwrap();
    assert_eq!(r.id, None);
    assert!(r.is_enabled);
    assert!(r.scope_title && r.scope_content);
    assert!(!r.is_regex);
    assert_eq!(r.timeout_millisecond, 1000);
    assert_eq!(r.scope.as_deref(), Some("某书"));
}

// 显式给出的作用域标记不会被默认值覆盖
#[test]
fn import_respects_explicit_scope_flags() {
    let r = normalize_import_rule(serde_json::json!({
        "pattern": "x",
        "replacement": "y",
        "scopeTitle": false,
        "scopeContent": true
    }))
    .unwrap();
    assert!(!r.scope_title);
    assert!(r.scope_content);
}

// 包装对象 `{name, rules:[...]}` 与裸数组两种规则包都能解析
#[test]
fn import_accepts_wrapper_and_array() {
    let wrapped = serde_json::json!({
        "name": "我的规则包",
        "rules": [{"pattern": "a", "replacement": "b"}]
    });
    assert_eq!(import_items(wrapped).unwrap().len(), 1);
    let arr = serde_json::json!([
        {"pattern": "a", "replacement": "b"},
        {"pattern": "c", "replacement": "d"}
    ]);
    assert_eq!(import_items(arr).unwrap().len(), 2);
}

// 预览要返回每一步的中间值，而不只是最终值
#[test]
fn preview_records_intermediate_values() {
    let rules = vec![
        mk("章号", r"第(\d+)章", "Chapter$1", true),
        mk("去广告", "广告", "", false),
    ];
    let (out, steps) = preview_apply(&rules, "第12章 广告内容", "", "", false);
    assert_eq!(out, "Chapter12 内容");
    assert_eq!(steps.len(), 2);
    assert_eq!(steps[0].output, "Chapter12 广告内容");
    assert_eq!(steps[0].hits, 1);
    assert_eq!(steps[0].index, 0);
    assert!(steps[0].applied);
    assert_eq!(steps[1].output, out);
    assert_eq!(steps[1].hits, 1);
}

// 未启用/作用域不匹配的规则要记录跳过原因，不参与输出
#[test]
fn preview_reports_skipped_rules() {
    let mut disabled = mk("关", "x", "y", false);
    disabled.is_enabled = false;
    let mut wrong_scope = mk("限定", "x", "y", false);
    wrong_scope.scope = Some("别的书".into());
    let (out, steps) = preview_apply(&[disabled, wrong_scope], "x", "某书", "site", false);
    assert_eq!(out, "x");
    assert_eq!(steps[0].skipped_reason.as_deref(), Some("disabled"));
    assert_eq!(steps[1].skipped_reason.as_deref(), Some("scope_mismatch"));
    assert!(!steps[1].applied);
}

// 超长中间值按 UTF-8 边界截断，并标记 truncated
#[test]
fn preview_truncates_long_output() {
    let long = "中".repeat(PREVIEW_STEP_OUTPUT_MAX);
    let (_, steps) = preview_apply(&[mk("noop", "zzz", "", false)], &long, "", "", false);
    assert!(steps[0].output_truncated);
    assert!(steps[0].output.len() <= PREVIEW_STEP_OUTPUT_MAX);
}

// 新增的 page_width/font_family：旧客户端不传必须放行（保持向后兼容）
#[test]
fn settings_new_fields_are_optional_for_old_clients() {
    assert!(validate_settings(&SettingsQ::default()).is_ok());
    let legacy: SettingsQ = serde_json::from_value(serde_json::json!({
        "theme": "dark",
        "font_size": 16,
        "read_mode": "scroll"
    }))
    .unwrap();
    assert_eq!(legacy.page_width, None);
    assert_eq!(legacy.font_family, None);
    assert!(validate_settings(&legacy).is_ok());
}

// page_width 范围 320–2000，font_family 走枚举白名单；非法值 400
#[test]
fn settings_page_width_and_font_family_are_validated() {
    for bad in [319_i64, 2001] {
        let q = SettingsQ {
            page_width: Some(bad),
            ..Default::default()
        };
        assert!(validate_settings(&q).is_err(), "page_width={bad} 应被拒绝");
    }
    for ok in [320_i64, 800, 2000] {
        let q = SettingsQ {
            page_width: Some(ok),
            ..Default::default()
        };
        assert!(validate_settings(&q).is_ok(), "page_width={ok} 应通过");
    }
    for bad in ["comic", "Arial", ""] {
        let q = SettingsQ {
            font_family: Some(bad.into()),
            ..Default::default()
        };
        assert!(
            validate_settings(&q).is_err(),
            "font_family={bad:?} 应被拒绝"
        );
    }
    for ok in [
        "serif", "sans", "kaiti", "songti", "heiti", "mono", "system",
    ] {
        let q = SettingsQ {
            font_family: Some(ok.into()),
            ..Default::default()
        };
        assert!(validate_settings(&q).is_ok(), "font_family={ok} 应通过");
    }
}

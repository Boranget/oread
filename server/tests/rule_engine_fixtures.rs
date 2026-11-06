//! 规则引擎 fixture 测试（phase 2.1）。
//!
//! 每个目录是一组与实现无关的「输入 + 规则 + 期望输出」：
//!   tests/fixtures/rule_engine/<case>/meta.json
//!   tests/fixtures/rule_engine/<case>/input.html | input.txt | input.json | input.xml
//!   tests/fixtures/rule_engine/<case>/expected.json
//!
//! `meta.json` 字段：
//!   mode: "string" | "list"（默认 string）
//!   rule: 规则串
//!   is_url: bool（默认 false，仅 string 模式）
//!   base_url: string（默认 ""，仅 string 模式）
//!
//! 这里只走 `AnalyzeRule` 的公开入口，拆分 `rule_engine.rs` 后仍然有效。

use std::path::{Path, PathBuf};

use boranlib::rule_engine::AnalyzeRule;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Meta {
    #[serde(default = "default_mode")]
    mode: String,
    rule: String,
    #[serde(default)]
    is_url: bool,
    #[serde(default)]
    base_url: String,
    #[serde(default)]
    description: String,
}

fn default_mode() -> String {
    "string".into()
}

fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("rule_engine")
}

fn read_input(dir: &Path) -> String {
    for name in ["input.html", "input.json", "input.xml", "input.txt"] {
        let path = dir.join(name);
        if path.is_file() {
            return std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("读取 {} 失败: {e}", path.display()));
        }
    }
    panic!("{} 缺少 input.* 文件", dir.display());
}

fn case_dirs() -> Vec<PathBuf> {
    let root = fixtures_root();
    let mut dirs: Vec<_> = std::fs::read_dir(&root)
        .unwrap_or_else(|e| panic!("读取 fixture 目录 {} 失败: {e}", root.display()))
        .filter_map(|entry| {
            let entry = entry.ok()?;
            entry.file_type().ok()?.is_dir().then(|| entry.path())
        })
        .collect();
    dirs.sort();
    assert!(!dirs.is_empty(), "{} 下没有 fixture", root.display());
    dirs
}

#[test]
fn rule_engine_fixtures() {
    let mut failed = Vec::new();
    let mut total = 0;
    for dir in case_dirs() {
        let name = dir.file_name().unwrap().to_string_lossy();
        total += 1;
        let meta_path = dir.join("meta.json");
        let meta_text = std::fs::read_to_string(&meta_path)
            .unwrap_or_else(|e| panic!("{name}: 读取 meta.json 失败: {e}"));
        let meta: Meta = serde_json::from_str(&meta_text)
            .unwrap_or_else(|e| panic!("{name}: meta.json 解析失败: {e}"));
        let input = read_input(&dir);
        let expected_text = std::fs::read_to_string(dir.join("expected.json"))
            .unwrap_or_else(|e| panic!("{name}: 读取 expected.json 失败: {e}"));
        let ar = AnalyzeRule::new();

        let outcome = match meta.mode.as_str() {
            "string" => {
                let expected: String = serde_json::from_str(&expected_text)
                    .unwrap_or_else(|e| panic!("{name}: expected.json 应为字符串: {e}"));
                let actual = ar
                    .get_string(&input, &meta.rule, meta.is_url, &meta.base_url)
                    .map_err(|e| e.to_string());
                match actual {
                    Ok(actual) => compare_scalar(&name, &meta, &expected, &actual),
                    Err(err) => Err(format!("{name}: 规则报错: {err}")),
                }
            }
            "list" => {
                let expected: Vec<String> = serde_json::from_str(&expected_text)
                    .unwrap_or_else(|e| panic!("{name}: expected.json 应为字符串数组: {e}"));
                match ar.get_string_list(&input, &meta.rule) {
                    Ok(actual) => compare_list(&name, &meta, &expected, &actual),
                    Err(err) => Err(format!("{name}: 规则报错: {err}")),
                }
            }
            other => Err(format!("{name}: 未知 mode {other:?}")),
        };
        if let Err(msg) = outcome {
            failed.push(msg);
        }
    }

    assert!(
        failed.is_empty(),
        "{} / {} 个 fixture 失败:\n{}\n\nfixture 目录: {}",
        failed.len(),
        total,
        failed.join("\n"),
        fixtures_root().display()
    );
}

fn compare_scalar(name: &str, meta: &Meta, expected: &str, actual: &str) -> Result<(), String> {
    if actual == expected {
        Ok(())
    } else {
        Err(format!(
            "{name}: {}（rule={:?}）\n  expected: {expected:?}\n  actual:   {actual:?}",
            meta.description, meta.rule
        ))
    }
}

fn compare_list(
    name: &str,
    meta: &Meta,
    expected: &[String],
    actual: &[String],
) -> Result<(), String> {
    if actual == expected {
        Ok(())
    } else {
        Err(format!(
            "{name}: {}（rule={:?}）\n  expected: {expected:?}\n  actual:   {actual:?}",
            meta.description, meta.rule
        ))
    }
}

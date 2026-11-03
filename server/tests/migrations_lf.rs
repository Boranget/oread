//! 防回归：迁移文件必须始终是 LF 字节。
//!
//! `sqlx::migrate!` 用 `include_str!` 把 `.sql` 的文件字节原样内嵌进二进制，并据此计算
//! 校验和存进 `_sqlx_migrations`。如果 checkout 按 CRLF 落地（Windows `core.autocrlf=true`），
//! 二进制里内嵌的就是「多 4 个字节」的版本，与库中 LF 校验和不一致 → 启动即
//! `VersionMismatch`。v0.3.1 的事故就是 Windows 产物内嵌 0003 为 414 字节、Linux 为 410。
//!
//! 仓库根目录的 `.gitattributes` 已声明 `server/migrations/** text eol=lf`；本测试是
//! 第二道闸：无论 CI 还是本地，只要磁盘文件或内嵌字节里出现 `\r` 就直接失败。

use std::fs;
use std::path::{Path, PathBuf};

fn collect_sql(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("读取目录失败 {}: {e}", dir.display()));
    for entry in entries {
        let path = entry.expect("读取目录项失败").path();
        if path.is_dir() {
            collect_sql(&path, out);
        } else if path.extension().and_then(|s| s.to_str()) == Some("sql") {
            out.push(path);
        }
    }
}

/// 定位某个 migration 对应的磁盘文件（文件名以 `{version:04}_` 开头）。
fn find_migration_file(dir: &Path, version: i64) -> Option<PathBuf> {
    let prefix = format!("{version:04}_");
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let entries = fs::read_dir(&current).ok()?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with(&prefix))
                .unwrap_or(false)
            {
                return Some(path);
            }
        }
    }
    None
}

fn describe_cr(bytes: &[u8], pos: usize) -> String {
    let line = bytes[..pos].iter().filter(|b| **b == b'\n').count() + 1;
    format!("字节偏移 {pos}（第 {line} 行），文件长度 {}", bytes.len())
}

#[test]
fn migrations_on_disk_have_no_cr() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
    let mut files = Vec::new();
    collect_sql(&root, &mut files);
    assert!(
        !files.is_empty(),
        "没有找到任何迁移文件：{}",
        root.display()
    );
    files.sort();

    let mut failures = Vec::new();
    for path in &files {
        let bytes = fs::read(path).unwrap_or_else(|e| panic!("读取失败 {}: {e}", path.display()));
        if let Some(pos) = bytes.iter().position(|b| *b == b'\r') {
            failures.push(format!("{}：首个 \\r 在 {}", path.display(), describe_cr(&bytes, pos)));
        }
    }
    assert!(
        failures.is_empty(),
        "迁移文件必须是 LF（CRLF 会让 sqlx 校验和跨平台不一致，见 .gitattributes）：\n{}",
        failures.join("\n")
    );
}

fn assert_embedded_lf(migrator: &sqlx::migrate::Migrator, dir: &Path, label: &str) {
    let mut failures = Vec::new();

    for migration in migrator.iter() {
        let sql = migration.sql.as_ref();
        let cr = sql.bytes().filter(|b| *b == b'\r').count();
        // 这条 `--nocapture` 输出是「Windows 产物内嵌 0003 len=410」的证据来源
        eprintln!(
            "[embedded:{label}] {:04}_{} len={} cr={}",
            migration.version,
            migration.description,
            sql.len(),
            cr
        );

        if let Some(pos) = sql.find('\r') {
            failures.push(format!(
                "{label} {:04}_{}：内嵌 SQL 含 \\r，位置 {}",
                migration.version,
                migration.description,
                describe_cr(sql.as_bytes(), pos)
            ));
        }

        if let Some(path) = find_migration_file(dir, migration.version) {
            let disk = fs::read(&path).unwrap_or_else(|e| panic!("读取失败 {}: {e}", path.display()));
            if disk != sql.as_bytes() {
                failures.push(format!(
                    "{label} {:04}_{}：内嵌字节与磁盘文件不一致（内嵌 {} / 磁盘 {}）",
                    migration.version,
                    migration.description,
                    sql.len(),
                    disk.len()
                ));
            }
        } else {
            failures.push(format!(
                "{label} {:04}_{}：在 {} 找不到对应的磁盘文件",
                migration.version,
                migration.description,
                dir.display()
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "sqlx::migrate! 内嵌的迁移字节必须与 LF 磁盘文件一致：\n{}",
        failures.join("\n")
    );
}

#[test]
fn embedded_migrations_have_no_cr() {
    let base = Path::new(env!("CARGO_MANIFEST_DIR"));

    let shared = sqlx::migrate!("./migrations/shared");
    assert_embedded_lf(&shared, &base.join("migrations/shared"), "shared");

    let user = sqlx::migrate!("./migrations/user");
    assert_embedded_lf(&user, &base.join("migrations/user"), "user");
}

//! db_guard 单测（move-only 从 db_guard.rs 拆出）。

use super::*;

use sqlx::migrate::{Migration, MigrationType};
use std::borrow::Cow;
use std::sync::atomic::{AtomicU32, Ordering};

static SEQ: AtomicU32 = AtomicU32::new(0);

fn tmp_dir(tag: &str) -> PathBuf {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("oread-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn test_cfg(dir: &Path, keep: usize) -> Config {
    let mut cfg = Config::from_env();
    cfg.data_dir = dir.display().to_string();
    cfg.db_backup_keep = keep;
    cfg.migration_repair = true;
    cfg
}

/// 用指定 SQL 构造迁移集（校验和按字节计算，和 sqlx 一致）。
fn migrator_with_sql(rows: &[(i64, &'static str)]) -> Migrator {
    let migrations: Vec<Migration> = rows
        .iter()
        .map(|(version, sql)| {
            Migration::new(
                *version,
                Cow::Borrowed("test migration"),
                MigrationType::Simple,
                Cow::Borrowed(sql),
            )
        })
        .collect();
    Migrator {
        migrations: Cow::Owned(migrations),
        ignore_missing: false,
        locking: true,
    }
}

async fn stored_checksum(pool: &SqlitePool, version: i64) -> Vec<u8> {
    sqlx::query_scalar::<_, Vec<u8>>("SELECT checksum FROM _sqlx_migrations WHERE version = ?1")
        .bind(version)
        .fetch_one(pool)
        .await
        .unwrap()
}

fn backup_dir_count(cfg: &Config) -> usize {
    std::fs::read_dir(backup_root(cfg))
        .map(|entries| entries.flatten().filter(|e| e.path().is_dir()).count())
        .unwrap_or(0)
}

async fn open_pool(path: &Path) -> SqlitePool {
    let opts = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true);
    SqlitePool::connect_with(opts).await.unwrap()
}

async fn create_migrations_table(pool: &SqlitePool, rows: &[(i64, bool, Vec<u8>)]) {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS _sqlx_migrations (\n  version BIGINT PRIMARY KEY,\n  \
         description TEXT NOT NULL,\n  installed_on TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,\n  \
         success BOOLEAN NOT NULL,\n  checksum BLOB NOT NULL,\n  execution_time BIGINT NOT NULL\n)",
    )
    .execute(pool)
    .await
    .unwrap();
    for (version, success, checksum) in rows {
        sqlx::query(
            "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) \
             VALUES (?1, 'test', ?2, ?3, 0)",
        )
        .bind(*version)
        .bind(if *success { 1i64 } else { 0i64 })
        .bind(checksum.as_slice())
        .execute(pool)
        .await
        .unwrap();
    }
}

fn migrator(versions: &[i64]) -> Migrator {
    let migrations: Vec<Migration> = versions
        .iter()
        .map(|v| {
            Migration::new(
                *v,
                Cow::Borrowed("test migration"),
                MigrationType::Simple,
                Cow::Borrowed("SELECT 1;"),
            )
        })
        .collect();
    Migrator {
        migrations: Cow::Owned(migrations),
        ignore_missing: false,
        locking: true,
    }
}

/// 备份必须包含 WAL 中未 checkpoint 的数据，且产物能过 integrity_check。
#[tokio::test]
async fn backup_is_consistent_with_wal() {
    let dir = tmp_dir("backup-wal");
    let cfg = test_cfg(&dir, 5);
    let src = dir.join("shared.db");
    let pool = open_pool(&src).await;
    sqlx::query("PRAGMA journal_mode=WAL")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("CREATE TABLE t (x TEXT)")
        .execute(&pool)
        .await
        .unwrap();
    let blob = "x".repeat(2048);
    for _ in 0..60 {
        sqlx::query("INSERT INTO t (x) VALUES (?1)")
            .bind(&blob)
            .execute(&pool)
            .await
            .unwrap();
    }
    // 前提：数据确实还留在 WAL 里（只拷主文件会丢行）。
    let wal = src.with_extension("db-wal");
    let wal_len = std::fs::metadata(&wal).map(|m| m.len()).unwrap_or(0);
    assert!(wal_len > 0, "测试前提：WAL 中应有未合并的数据");

    let backup_dir = backup_now(&cfg, Scope::Shared).await.unwrap();
    let copy = backup_dir.join("shared.db");
    assert!(copy.is_file(), "备份文件应存在");

    let opts = SqliteConnectOptions::new().filename(&copy).read_only(true);
    let mut conn = SqliteConnection::connect_with(&opts).await.unwrap();
    let verdict: String = sqlx::query_scalar("PRAGMA integrity_check")
        .fetch_one(&mut conn)
        .await
        .unwrap();
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM t")
        .fetch_one(&mut conn)
        .await
        .unwrap();
    assert_eq!(verdict, "ok");
    assert_eq!(rows, 60, "WAL 中的行也必须进入快照");
    let _ = conn.close().await;
    pool.close().await;
    let _ = std::fs::remove_dir_all(&dir);
}

/// 保留策略：造 7 份后只剩最近 5 份，且不误删非备份目录。
#[test]
fn retention_keeps_newest_five() {
    let dir = tmp_dir("retention");
    let cfg = test_cfg(&dir, 5);
    let root = backup_root(&cfg);
    std::fs::create_dir_all(&root).unwrap();
    for i in 0..7u32 {
        std::fs::create_dir(root.join(format!("20261104T00000{i}Z"))).unwrap();
    }
    std::fs::create_dir(root.join("manual-keep")).unwrap();

    prune_backups(&cfg);

    let mut kept: Vec<String> = std::fs::read_dir(&root)
        .unwrap()
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    kept.sort();
    assert_eq!(kept.len(), 6, "5 份备份 + 1 个人工目录");
    for i in 0..2u32 {
        assert!(
            !root.join(format!("20261104T00000{i}Z")).exists(),
            "最旧的第 {i} 份应被清理"
        );
    }
    for i in 2..7u32 {
        assert!(
            root.join(format!("20261104T00000{i}Z")).exists(),
            "最近的第 {i} 份应保留"
        );
    }
    assert!(root.join("manual-keep").exists(), "非备份目录不得删除");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 状态探测：库比程序新 / 有待应用迁移 / 全新库。
#[tokio::test]
async fn inspect_detects_newer_pending_and_fresh() {
    let dir = tmp_dir("inspect");
    let db = dir.join("shared.db");
    let pool = open_pool(&db).await;

    assert_eq!(
        inspect(&pool, &migrator(&[1, 2])).await.unwrap(),
        DbState::Fresh
    );

    create_migrations_table(
        &pool,
        &[
            (1, true, vec![0u8; 48]),
            (2, true, vec![0u8; 48]),
            (3, true, vec![0u8; 48]),
        ],
    )
    .await;
    assert_eq!(
        inspect(&pool, &migrator(&[1, 2])).await.unwrap(),
        DbState::Newer {
            applied_max: 3,
            supported_max: 2
        }
    );

    pool.close().await;
    let _ = std::fs::remove_dir_all(&dir);

    let dir = tmp_dir("inspect-pending");
    let db = dir.join("shared.db");
    let pool = open_pool(&db).await;
    create_migrations_table(&pool, &[(1, true, vec![0u8; 48])]).await;
    assert_eq!(
        inspect(&pool, &migrator(&[1, 2])).await.unwrap(),
        DbState::Pending {
            applied_max: 1,
            pending: vec![2]
        }
    );
    pool.close().await;
    let _ = std::fs::remove_dir_all(&dir);
}

/// 真实迁移入口：库比程序新时给出人话错误，且备份目录里确实有一份快照。
#[tokio::test]
async fn newer_database_aborts_with_backup() {
    let dir = tmp_dir("newer-abort");
    let cfg = test_cfg(&dir, 5);
    let db = dir.join("shared.db");
    let pool = open_pool(&db).await;
    create_migrations_table(
        &pool,
        &[
            (1, true, vec![0u8; 48]),
            (2, true, vec![0u8; 48]),
            (3, true, vec![0u8; 48]),
        ],
    )
    .await;

    let err = migrate_bucket(&cfg, &pool, &migrator(&[1, 2]), Scope::Shared)
        .await
        .unwrap_err();
    assert!(err.0.contains("数据库版本高于当前程序"), "{}", err.0);
    assert!(err.0.contains("数据库版本：3"), "{}", err.0);
    assert!(err.0.contains("本程序支持的最高版本：2"), "{}", err.0);
    assert!(
        !err.0.contains("VersionMismatch"),
        "不得暴露裸错误：{}",
        err.0
    );

    let backups: Vec<_> = std::fs::read_dir(backup_root(&cfg))
        .unwrap()
        .flatten()
        .filter(|e| e.path().is_dir())
        .collect();
    assert_eq!(backups.len(), 1, "应恰好产生一份备份");
    let copy = backups[0].path().join("shared.db");
    assert!(copy.is_file());
    pool.close().await;
    let _ = std::fs::remove_dir_all(&dir);
}

/// 正常启动（无待应用迁移）不产生多余备份。
#[tokio::test]
async fn up_to_date_does_not_backup() {
    let dir = tmp_dir("uptodate");
    let cfg = test_cfg(&dir, 5);
    let db = dir.join("shared.db");
    let pool = open_pool(&db).await;

    let m = migrator(&[1]);
    let checksum = m.iter().next().unwrap().checksum.to_vec();
    create_migrations_table(&pool, &[(1, true, checksum)]).await;
    migrate_bucket(&cfg, &pool, &m, Scope::Shared)
        .await
        .unwrap();

    assert!(
        !backup_root(&cfg).exists(),
        "无待应用迁移时不应创建备份目录"
    );
    pool.close().await;
    let _ = std::fs::remove_dir_all(&dir);
}

/// 版本不匹配 / 权限 / 磁盘满都要翻译成人话，不打印裸枚举。
#[test]
fn errors_are_human_readable() {
    let dir = tmp_dir("messages");
    let cfg = test_cfg(&dir, 5);

    let mismatch = explain(
        &MigrateError::VersionMismatch(1),
        &cfg,
        Scope::Shared,
        7,
        7,
        None,
    );
    assert!(mismatch.contains("数据库迁移记录与当前程序不一致"));
    assert!(mismatch.contains("校验和"));
    assert!(mismatch.contains("backups"));
    assert!(!mismatch.contains("VersionMismatch"));

    let newer = newer_message(&cfg, Scope::Shared, 7, 5, None);
    assert!(newer.contains("数据库已被更新版本的程序写入"));
    assert!(newer.contains(env!("CARGO_PKG_VERSION")));

    let info = common_info(&cfg, Scope::Shared, 7, 5);
    assert!(info.contains("本程序支持的最高版本：5"));
    assert!(info.contains("数据库版本：7"));

    // 磁盘满 / 只读 / 语法错误：每条都要有原因与下一步。
    let (cause, next) = code_hint(Some(13));
    assert!(cause.contains("磁盘空间不足"));
    assert!(next.contains("BORANLIB_DB_BACKUP_KEEP"));
    let (cause, _) = code_hint(Some(8));
    assert!(cause.contains("没有写权限"));
    let (cause, _) = code_hint(Some(1));
    assert!(cause.contains("语法"));

    let msg = execute_message(&sqlx::Error::Protocol("synthetic".into()), &info);
    assert!(msg.contains("数据库迁移失败"));
    assert!(msg.contains("技术细节"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// 1) 库里是 CRLF 哈希、内嵌文件是 LF：启动成功、记录被改写、第二次启动无写入。
#[tokio::test]
async fn crlf_only_mismatch_is_normalized_once() {
    let dir = tmp_dir("repair-crlf");
    let cfg = test_cfg(&dir, 5);
    let db = dir.join("shared.db");
    let pool = open_pool(&db).await;

    let sql = "CREATE TABLE t (x TEXT);\nINSERT INTO t (x) VALUES ('a');\n";
    let m = migrator_with_sql(&[(1, sql)]);
    let lf = sha384(sql.as_bytes());
    let crlf = sha384(&crlf_bytes(sql.as_bytes()));
    assert_ne!(lf, crlf, "测试前提：LF/CRLF 哈希应不同");
    create_migrations_table(&pool, &[(1, true, crlf)]).await;

    migrate_bucket(&cfg, &pool, &m, Scope::Shared)
        .await
        .unwrap();
    assert_eq!(stored_checksum(&pool, 1).await, lf, "应改写成 LF 哈希");
    assert_eq!(backup_dir_count(&cfg), 1, "归一化前应有一份快照");

    // 第二次启动：不再改库、不再产生新备份。
    let before = stored_checksum(&pool, 1).await;
    migrate_bucket(&cfg, &pool, &m, Scope::Shared)
        .await
        .unwrap();
    assert_eq!(stored_checksum(&pool, 1).await, before);
    assert_eq!(backup_dir_count(&cfg), 1, "第二次启动不应新建备份");
    pool.close().await;
    let _ = std::fs::remove_dir_all(&dir);
}

/// 2) 内容真被改（非换行）：仍然拒绝启动、错误可读、不写库。
#[tokio::test]
async fn real_content_change_is_still_rejected() {
    let dir = tmp_dir("repair-tampered");
    let cfg = test_cfg(&dir, 5);
    let db = dir.join("shared.db");
    let pool = open_pool(&db).await;

    let sql = "CREATE TABLE t (x TEXT);\n";
    let m = migrator_with_sql(&[(1, sql)]);
    let tampered = sha384(b"CREATE TABLE t (x TEXT);\n-- tampered\n");
    assert_ne!(tampered, sha384(&crlf_bytes(sql.as_bytes())));
    create_migrations_table(&pool, &[(1, true, tampered.clone())]).await;

    let err = migrate_bucket(&cfg, &pool, &m, Scope::Shared)
        .await
        .unwrap_err();
    assert!(
        err.0.contains("数据库迁移记录与当前程序不一致"),
        "{}",
        err.0
    );
    assert!(err.0.contains("已尝试“换行归一化”"), "{}", err.0);
    assert_eq!(stored_checksum(&pool, 1).await, tampered, "真差异不得写库");
    assert_eq!(backup_dir_count(&cfg), 0, "真差异不应产生备份");
    pool.close().await;
    let _ = std::fs::remove_dir_all(&dir);
}

/// 3) WAL 模式下归一化 + 迁移前备份的一致性。
#[tokio::test]
async fn repair_in_wal_keeps_backup_consistent() {
    let dir = tmp_dir("repair-wal");
    let cfg = test_cfg(&dir, 5);
    let db = dir.join("shared.db");
    let pool = open_pool(&db).await;
    sqlx::query("PRAGMA journal_mode=WAL")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("CREATE TABLE data (x TEXT)")
        .execute(&pool)
        .await
        .unwrap();
    let blob = "x".repeat(1024);
    for _ in 0..30 {
        sqlx::query("INSERT INTO data (x) VALUES (?1)")
            .bind(&blob)
            .execute(&pool)
            .await
            .unwrap();
    }
    let wal_len = std::fs::metadata(db.with_extension("db-wal"))
        .map(|m| m.len())
        .unwrap_or(0);
    assert!(wal_len > 0, "测试前提：WAL 中应有未合并数据");

    let sql = "CREATE TABLE t (x TEXT);\n";
    let m = migrator_with_sql(&[(1, sql)]);
    let crlf = sha384(&crlf_bytes(sql.as_bytes()));
    create_migrations_table(&pool, &[(1, true, crlf)]).await;

    migrate_bucket(&cfg, &pool, &m, Scope::Shared)
        .await
        .unwrap();
    assert_eq!(stored_checksum(&pool, 1).await, sha384(sql.as_bytes()));
    assert_eq!(backup_dir_count(&cfg), 1);

    let backups: Vec<PathBuf> = std::fs::read_dir(backup_root(&cfg))
        .unwrap()
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.path())
        .collect();
    let copy = backups[0].join("shared.db");
    let opts = SqliteConnectOptions::new().filename(&copy).read_only(true);
    let mut conn = SqliteConnection::connect_with(&opts).await.unwrap();
    let verdict: String = sqlx::query_scalar("PRAGMA integrity_check")
        .fetch_one(&mut conn)
        .await
        .unwrap();
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM data")
        .fetch_one(&mut conn)
        .await
        .unwrap();
    assert_eq!(verdict, "ok");
    assert_eq!(rows, 30, "WAL 中的数据必须进入一致性快照");
    let _ = conn.close().await;
    pool.close().await;
    let _ = std::fs::remove_dir_all(&dir);
}

/// 4) 开关 off：退回“只报错不修”。
#[tokio::test]
async fn repair_switch_off_only_errors() {
    let dir = tmp_dir("repair-off");
    let mut cfg = test_cfg(&dir, 5);
    cfg.migration_repair = false;
    let db = dir.join("shared.db");
    let pool = open_pool(&db).await;

    let sql = "CREATE TABLE t (x TEXT);\n";
    let m = migrator_with_sql(&[(1, sql)]);
    let crlf = sha384(&crlf_bytes(sql.as_bytes()));
    create_migrations_table(&pool, &[(1, true, crlf.clone())]).await;

    let err = migrate_bucket(&cfg, &pool, &m, Scope::Shared)
        .await
        .unwrap_err();
    assert!(
        err.0.contains("数据库迁移记录与当前程序不一致"),
        "{}",
        err.0
    );
    assert!(!err.0.contains("已尝试“换行归一化”"), "{}", err.0);
    assert_eq!(stored_checksum(&pool, 1).await, crlf, "off 时不得写库");
    assert_eq!(backup_dir_count(&cfg), 0);
    pool.close().await;
    let _ = std::fs::remove_dir_all(&dir);
}

use std::env;

/// 服务配置，全部来自环境变量，便于在 100MB 内存的小机上调参。
#[derive(Clone, Debug)]
pub struct Config {
    pub host: String,
    pub port: u16,
    /// 数据根目录，下面会生成 shared.db 与 users/{uid}.db
    pub data_dir: String,
    pub jwt_secret: String,
}

impl Config {
    pub fn from_env() -> Self {
        Self {
            host: env::var("HOST").unwrap_or_else(|_| "0.0.0.0".into()),
            port: env::var("PORT").ok().and_then(|v| v.parse().ok()).unwrap_or(8080),
            data_dir: env::var("DATA_DIR").unwrap_or_else(|_| "data".into()),
            jwt_secret: env::var("JWT_SECRET").unwrap_or_else(|_| "change-me-in-prod".into()),
        }
    }

    /// 共享桶连接串：sqlite://<data_dir>/shared.db
    pub fn shared_db_url(&self) -> String {
        format!("sqlite://{}/shared.db", self.data_dir)
    }

    /// 用户独立桶连接串：sqlite://<data_dir>/users/<uid>.db（物理分桶）
    pub fn user_db_url(&self, uid: &str) -> String {
        format!("sqlite://{}/users/{}.db", self.data_dir, uid)
    }
}

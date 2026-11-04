-- 最后登录时间：仅记录网页/API 登录（/api/auth/login）；/k 表单登录走独立链路，不在此记录
ALTER TABLE users ADD COLUMN last_login_at TEXT;

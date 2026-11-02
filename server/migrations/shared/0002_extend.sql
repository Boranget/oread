-- 完整书源对象（含全部字段与五类规则子对象）序列化为 JSON 存放，便于直接进出 Rust 结构体
ALTER TABLE book_sources ADD COLUMN content TEXT;

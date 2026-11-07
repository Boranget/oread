// 书源展示名：后端字段缺失时统一回退，避免各处各写一份。
export function sourceName(s) {
  return (s && s.book_source_name) || '未命名书源';
}

// 后端 type/type_name 正在加：有就显示，没有就跳过，不造字段
export function sourceType(s) {
  return (s && (s.type_name || s.type)) || '';
}

// ignored_source_ids 可能是 URL，也可能是 1-based 序号；能映射到书源名就显示名字
export function sourceLabel(sources, id) {
  const s = (sources || []).find((x) => x.book_source_url === id);
  if (s) {
    const t = sourceType(s);
    return t ? `${sourceName(s)}（${t}）` : sourceName(s);
  }
  return /^\d+$/.test(id) ? `第 ${id} 个源（序号）` : id;
}

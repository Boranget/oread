// 查询串构造：空值不写进查询串，保持 /search?key=&page=1 这类请求干净。
export function buildQuery(obj) {
  const qs = new URLSearchParams();
  Object.entries(obj).forEach(([k, v]) => {
    if (v !== undefined && v !== null && v !== '') qs.set(k, String(v));
  });
  return qs.toString();
}

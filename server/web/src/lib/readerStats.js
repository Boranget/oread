// 阅读统计：本会话累计秒数/字数，按 60s 心跳增量上报；失败保留计数下次重试。
// 抽成工厂便于 node --test 验证去重、并发闸门与成功后扣减。

export function createReadingStats({
  recordReading,
  getBookId,
  getName = () => '',
  getChapterIndex,
  getTotalChapters = () => 0,
  isVisible = () => true,
} = {}) {
  let seconds = 0;
  let chars = 0;
  let busy = false;
  const seen = new Set();

  // 每秒调用：仅前台累计阅读时长，满 60s 心跳上报
  function tick() {
    if (isVisible()) seconds += 1;
    if (seconds >= 60) flush();
  }

  // 同一章只计一次字数（避免重试/回看重复累计）
  function addChapter(pos, charCount) {
    if (pos < 0 || seen.has(pos)) return;
    seen.add(pos);
    chars += Math.max(0, Number(charCount) || 0);
  }

  function flush(keepalive = false) {
    const pos = getChapterIndex();
    const bookId = getBookId();
    if (busy || !bookId || pos < 0) return Promise.resolve(false);
    if (seconds < 1 && chars < 1) return Promise.resolve(false);
    const reportSeconds = Math.min(86400, seconds);
    const reportChars = Math.min(10000000, chars);
    busy = true;
    return Promise.resolve(
      recordReading(
        {
          book_id: bookId,
          name: getName(),
          seconds: reportSeconds,
          chars: reportChars,
          chapter_index: pos,
          total_chapters: getTotalChapters(),
        },
        keepalive
      )
    )
      .then(() => {
        seconds = Math.max(0, seconds - reportSeconds);
        chars = Math.max(0, chars - reportChars);
        return true;
      })
      .catch(() => false)
      .finally(() => {
        busy = false;
      });
  }

  return { tick, addChapter, flush };
}

// 本地 Mock 书源服务：用于端到端验证「抓取 -> 规则解析」管线（无需外网）。
const http = require('http');
const url = require('url');

function j(res, obj) {
  res.writeHead(200, { 'Content-Type': 'application/json; charset=utf-8' });
  res.end(JSON.stringify(obj));
}

const server = http.createServer((req, res) => {
  const u = url.parse(req.url, true);
  const p = u.pathname;
  const key = u.query.key || '';

  if (p === '/search') {
    return j(res, {
      list: [
        { name: `《${key}》示例`, author: '佚名', book_url: '/book/b1', cover_url: '', intro: `这是关于《${key}》的简介。` },
        { name: `${key}外传`, author: '某人', book_url: '/book/b2', cover_url: '', intro: '另一本示例书。' },
      ],
    });
  }
  if (p === '/explore') {
    return j(res, {
      list: [
        { name: '热门书一', author: '甲', book_url: '/book/b1', cover_url: '', intro: '发现页推荐。' },
        { name: '热门书二', author: '乙', book_url: '/book/b2', cover_url: '', intro: '发现页推荐。' },
      ],
    });
  }
  if (p === '/book/b1' || p === '/book/b2') {
    return j(res, {
      name: p === '/book/b1' ? '示例书' : '外传',
      author: '佚名',
      intro: '本书的详细介绍……',
      toc_url: `/toc${p.slice(5)}`,
      cover_url: '',
    });
  }
  if (p.startsWith('/toc')) {
    return j(res, {
      chapters: [
        { title: '第一章 起始', url: `/content${p}/0` },
        { title: '第二章 发展', url: `/content${p}/1` },
        { title: '第三章 高潮', url: `/content${p}/2` },
      ],
    });
  }
  if (p.startsWith('/content')) {
    return j(res, {
      title: '章节标题',
      content: '这是一段正文内容。\n山有木兮木有枝，心悦君兮君不知。\n夜深风竹敲秋韵，万叶千声皆是恨。\n（示例正文结束）',
    });
  }
  res.writeHead(404); res.end('not found');
});

server.listen(8799, '127.0.0.1', () => console.log('mock book api on http://127.0.0.1:8799'));
